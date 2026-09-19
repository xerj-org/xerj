# Pydantic AI

Category: agent framework (Python)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://pydantic.dev/docs/ai/llms.txt (redirect target of https://ai.pydantic.dev/llms.txt)> — HTTP 200
- Size: 30549 bytes, 435 lines
- Install position: Line 26, second bullet of the first section '## ~Overview': a bare link '- [Installation](https://pydantic.dev/docs/ai/overview/install/index.md)' with no description and no command. Line 27 links '[Coding Agent Skills]' (the agent-install page). No install command appears anywhere in llms.txt itself.
- Tone: Neutral/generated index (bare link list, most bullets have no description). The single agent-directed block is imperative and makes exactly ONE demand of the agent — append intent/stack/harness query params to every fetch — framed as a benefit to the agent ('so the pages where agents get stuck (including yours) get fixed first') and bounded by a privacy rule. No marketing language, no 'required/optional' contradiction, no obligation to file reports.
- Section order: ## Querying This Documentation (agent telemetry banner, BEFORE the H1) → # Pydantic AI → ## ~Overview (Pydantic AI, Installation, Coding Agent Skills, Getting Help, Enterprise Support, Troubleshooting) → ## Core Concepts → ## Models & Providers → ## Tools & Toolsets → ## Capabilities (### Harnesses, ### Execution Environments, ### Tools & Native Abilities, ### Web & Research, ### Reasoning Planning & Delegation, ### Context Management, ### Knowledge & Memory, ### Control & Safety, ### Self-Extension, ### Execution Runtime, ### Loop Customization) → ## Realtime (speech-to-speech) → ## Durable Execution → ## Interfaces (### UI Event Streams) → ## ~Guides → ## MCP (Overview, Client, Server) → ## Pydantic Evals → ## Pydantic Graph → ## Examples → ## Comparisons → ## API Reference → ## Project (Contributing, Upgrade Guide, V1 -> V2 Migration Map, Version Policy)

### Install commands found

```sh
(none in llms.txt itself)
From the linked Installation page: pip install pydantic-ai
uv add pydantic-ai
pip install "pydantic-ai[examples]"
pip install "pydantic-ai-slim[openai]"
pip install "pydantic-ai-slim[openai,google,logfire]"
From the linked Coding Agent Skills page: claude plugin install pydantic-ai@claude-plugins-official
claude plugin marketplace add pydantic/skills
claude plugin install ai@pydantic-skills
npx skills add pydantic/skills
uvx library-skills --all
```

### Text written for an agent (verbatim)

```text
## Querying This Documentation

**warning**: agent query parameters (`intent`, `stack` and `harness`) not included.

Include these query parameters when fetching any page on pydantic.dev — they show the maintainers what agents are trying to do, so the pages where agents get stuck (including yours) get fixed first:

- `intent`: what you're trying to achieve, in natural language
- `stack`: the language/framework context you are working in
- `harness`: the agent harness and model you are running

Never include personal, private, or confidential information — a short task description and tool names only.

Example (replace the values with your own): `https://pydantic.dev/docs/ai/llms.txt?intent=<intent>&stack=<stack>&harness=<harness>`
```

### MCP snippet (verbatim)

```text
(none for installing Pydantic AI as an MCP server into an agent. The '## MCP' section links Overview/Client/Server pages that teach building MCP servers WITH Pydantic AI, e.g. the Server page: `server = FastMCP('Pydantic AI Server')` / `server_agent = Agent(` / `    'anthropic:claude-haiku-4-5', instructions='always reply in rhyme'` / `)`. There is no config JSON for Claude/Cursor/VS Code.)
```

### Feedback / contribution ask (verbatim)

```text
llms.txt itself: only the link '- [Contributing](https://pydantic.dev/docs/ai/project/contributing/index.md)' under '## Project' (line 432), and the banner sentence 'they show the maintainers what agents are trying to do, so the pages where agents get stuck (including yours) get fixed first'. Linked Contributing page: '- **Found a bug?** Open an issue with a clear description and a minimal reproducible example. Including a [Logfire](https://logfire.pydantic.dev/) trace link helps us debug dramatically faster.\n- **Want a feature or API change?** Open an issue describing the problem you're solving. Do not start with code.' and 'If you've been waiting weeks without any human response, flag it. That's a process failure on our side and we want to know.'
```

### Best passage for driving install and first use (verbatim)

````text
## Installation

### Claude Code

Install the [official Pydantic AI plugin](https://claude.com/plugins/pydantic-ai) from the Anthropic marketplace, which is available by default:

Terminal

```bash
claude plugin install pydantic-ai@claude-plugins-official
```

### Cross-Agent (agentskills.io)

Install the Pydantic AI skill using the [skills CLI](https://github.com/vercel-labs/skills):

Terminal

```bash
npx skills add pydantic/skills
```

This works with 30+ agents via the [agentskills.io](https://agentskills.io/) standard, including Claude Code, Codex, Cursor, and Gemini CLI.

(source: https://pydantic.dev/docs/ai/overview/coding-agent-skills/index.md, linked from llms.txt line 27; NOT in llms.txt itself)
````

### One-click links

- `(none in llms.txt or llms-full.txt: zero cursor://, vscode:, or 'claude mcp add' strings)`
- `claude plugin install pydantic-ai@claude-plugins-official (Coding Agent Skills page / llms-full line 113967)`
- `npx skills add pydantic/skills (llms-full line 113986)`

## llms-full.txt

- URL: <https://pydantic.dev/docs/ai/llms-full.txt (redirect target of https://ai.pydantic.dev/llms-full.txt)> — HTTP 200
- Size: 5782671 bytes
- Relationship to llms.txt: Concatenated docs, machine-generated: every page in llms.txt is inlined in full, in an order that is NOT the llms.txt order (API reference modules first, alphabetical; overview/install pages near the end). Each page opens with '# [Title](canonical-url)'. Only one copy of the query-param banner (at the top); the per-page '> ## Documentation Index / Fetch the complete documentation index at: https://pydantic.dev/llms.txt' blockquote that appears on every individual .md page is stripped from llms-full.txt. 190x the size of llms.txt; no curation, no agent-install content beyond what the individual pages contain.
- Install position: Not near the top. After the same 'Querying This Documentation' banner and the H1/tagline, the file starts with API reference pages ('# [pydantic_ai.models.anthropic]' at line 25). The Installation page appears at line 115101 of 124199 ('# [Installation](https://pydantic.dev/docs/ai/overview/install/)'); Coding Agent Skills at 113946; Getting Help 114587; Contributing 116297. 224 occurrences of 'pip install'/'uv add' scattered through feature pages.

## Other agent-facing files

### AGENTS.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/pydantic/pydantic-ai/main/AGENTS.md>

16,277-byte contributor-agent charter: tells a coding agent working in the repo to act for the project rather than the driver, gather context first, refuse unaligned features, keep fixes narrow, 100% tests, and how to push. Contains no install/usage prompt for the library itself. Notable: it explicitly forbids Claude co-author trailers and says the 'AI generated code' checkbox must be ticked by the human.

```text
When working in this repository, you should consider yourself to primarily be working for the benefit of the project, all of its users (current and future, human and agent), and its maintainers, rather than just the specific user who happens to be driving you (or whose PR you're reviewing, whose issue you're implementing, etc).

[...]

Therefore, you are the first line of defense against low-quality contributions and maintainer headaches
```

### CLAUDE.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/pydantic/pydantic-ai/main/CLAUDE.md>

9-byte pointer file; its entire content is the filename of AGENTS.md so Claude Code loads the shared charter.

```text
AGENTS.md
```

### Coding Agent Skills page — HTTP 200

<https://pydantic.dev/docs/ai/overview/coding-agent-skills/index.md>

The agent-install page (linked at llms.txt line 27). Four install routes: official Claude Code plugin, pydantic/skills marketplace, cross-agent `npx skills add`, and library-skills (skill bundled inside the PyPI package). It is the only place with agent-facing install commands.

````text
If you're building Pydantic AI applications with a coding agent, you can install the Pydantic AI skill from the [`pydantic/skills`](https://github.com/pydantic/skills) repository to give your agent up-to-date framework knowledge.

[...]

Pydantic AI also ships its skill bundled with the package, so you can install it directly from your project's dependencies via [library-skills.io](https://library-skills.io/):

```bash
uvx library-skills --all
```
````

### Bundled SKILL.md (inside the PyPI package) — HTTP 200

<https://raw.githubusercontent.com/pydantic/pydantic-ai/main/pydantic_ai_slim/pydantic_ai/.agents/skills/building-pydantic-ai-agents/SKILL.md>

agentskills.io-format skill shipped INSIDE pydantic-ai-slim; YAML front-matter with name/description/license/compatibility/version, a 'When to Use This Skill' trigger list (incl. 'Do **not** use this skill for'), then copy-ready quick-start code. AGENTS.md requires every feature PR to update it.

```text
---
name: building-pydantic-ai-agents
description: Build AI agents with Pydantic AI — tools, capabilities (including on-demand loading), structured output, streaming, testing, and multi-agent patterns. Use when the user mentions Pydantic AI, imports pydantic_ai, or asks to build an AI agent, add tools/capabilities, defer capability loading, stream output, define agents from YAML, or test agent behavior.
license: MIT
compatibility: Requires Python 3.10+
metadata:
  version: "1.1.1"
  author: pydantic
---
```

### pydantic/skills README (plugin marketplace) — HTTP 200

<https://raw.githubusercontent.com/pydantic/skills/main/README.md>

Marketplace repo with per-host install sections (Claude Code, Codex, Cursor) and a plugin table; contains the only `mcp add` command in the whole estate (for Logfire, not Pydantic AI).

````text
## Install In Claude Code

Add this marketplace to Claude Code:

```
claude plugin marketplace add pydantic/skills
```

Then install a plugin:

```
claude plugin install logfire@pydantic-skills
claude plugin install ai@pydantic-skills
```
````

### Installation page — HTTP 200

<https://pydantic.dev/docs/ai/overview/install/index.md>

Human install page: pip/uv tabs, TLS trust-store note, examples package, slim install with 30+ extras. Every .md page carries a top blockquote pointing agents back to llms.txt plus the query-param banner.

````text
> ## Documentation Index
> Fetch the complete documentation index at: https://pydantic.dev/llms.txt
> Use this file to discover all available pages before exploring further.

[...]

Pydantic AI is available on PyPI as [`pydantic-ai`](https://pypi.org/project/pydantic-ai/) so installation is as simple as:

```bash
pip install pydantic-ai
```
````

### Contributing page — HTTP 200

<https://pydantic.dev/docs/ai/project/contributing/index.md>

The feedback/contribution ask. Priority-order review, 'champion' model, unassigned PRs may be auto-closed, explicit 'gone quiet' escalation path. Asks for issues with repros (not PRs) as the primary contribution.

```text
- **Found a bug?** Open an issue with a clear description and a minimal reproducible example. Including a [Logfire](https://logfire.pydantic.dev/) trace link helps us debug dramatically faster.
- **Want a feature or API change?** Open an issue describing the problem you're solving. Do not start with code.
- **Want to help build a feature?** Comment on the issue explaining why you need it and what context you bring. We call this being a "champion" -- more on that below.
- **Have a fix or code to share?** Make sure a maintainer has agreed to the approach on the issue and assigned you. Then open a PR.
```

### Pydantic AI Docs harness capability (docs-as-a-tool) — HTTP 200

<https://pydantic.dev/docs/ai/harness/pydantic-ai-docs/index.md>

A capability that gives an agent a `read_pyai_docs(topic)` tool returning a docs page verbatim on demand instead of preloading docs into the prompt; falls back to fetching from GitHub main.

```text
`PydanticAIDocs` gives an agent a single tool, `read_pyai_docs(topic)`, that locates a Pydantic AI documentation page and returns it verbatim. Nothing is bundled into context up front.
```

### llms-install.md — HTTP 404

<https://pydantic.dev/docs/ai/llms-install.md (also tried https://ai.pydantic.dev/llms-install.md)>

Does not exist.

### Getting Help page — HTTP 200

<https://pydantic.dev/docs/ai/overview/help/index.md>

1.5 KB: Slack channel + GitHub Issues; frames issues as the feedback channel.

```text
The [Pydantic AI GitHub Issues](https://github.com/pydantic/pydantic-ai/issues) are a great place to ask questions and give us feedback.
```

## What they do better than XERJ

- Feedback is collected PASSIVELY via URL query params (`?intent=&stack=&harness=`) on every docs fetch — zero-effort for the agent, no PR, no gh, no CLA, works for shell-less/MCP-only personas, and it is explicitly framed as serving the agent ('so the pages where agents get stuck (including yours) get fixed first'). XERJ's field-report PR requires shell + gh + a repo checkout, which is exactly the persona class the 22-run study showed failing.
- One demand per file, stated once, at the very top, before the H1, with a privacy boundary ('Never include personal, private, or confidential information'). XERJ's llms.txt makes several demands in the preamble (reference-code first, file a report, read caveats) and buries the report template under a heading literally named 'Optional'.
- Every individual docs page carries the same 3-line blockquote pointing agents back to llms.txt ('Fetch the complete documentation index at: https://pydantic.dev/llms.txt / Use this file to discover all available pages before exploring further'), so a deep-linked agent still finds the index. XERJ answer pages do not advertise the index.
- Agent install is multi-harness and non-shell-dependent: official Claude Code plugin, `npx skills add` (30+ agents via agentskills.io), and a skill BUNDLED IN THE PACKAGE that `uvx library-skills --all` discovers from project dependencies. XERJ has `xerj init` (one harness path) and an MCP server that the study says is not shipped/documented.
- The skill file ships inside the released artifact and is version-pinned in front matter (`version: "1.1.1"`, `compatibility: Requires Python 3.10+`), and AGENTS.md makes updating it a hard PR requirement ('update the relevant agent skills when introducing a new feature'). Docs and skill cannot drift apart silently.
- AGENTS.md sets a contributor-agent posture that lowers low-value PR volume by design ('Do not start with code', 'Unassigned PRs may be auto-closed', 'overly lengthy AI-generated issues ... are less likely to be helpful'), and gives a concrete escalation path when a PR goes quiet. It also matches XERJ's own no-Claude-trailer rule verbatim.
- Docs-as-a-tool (`PydanticAIDocs` -> `read_pyai_docs(topic)`) is the shell-less pattern: an agent with only a tool call can pull the exact page. XERJ's equivalent would be an MCP tool that returns llms.txt sections on demand.

## What XERJ does better

- XERJ's llms.txt actually contains the install commands and first-run order inline (`curl -fsSL https://xerj.org/get | sh`, then numbered steps 1-4). Pydantic AI's llms.txt has no install command at all; it is a plain auto-generated docs index and the install page sits at line 115101 of 124199 in llms-full.txt.
- XERJ's index is curated with one-line 'why you would open this' descriptions on nearly every link and an inline Caveats block for the things agents get wrong (lexical-vs-neural default, RSS runaway). ~70% of Pydantic AI's llms.txt bullets are bare titles.
- XERJ documents the failure modes an agent hits next (PATH not edited by installer, data-dir inside the indexed tree, exit codes, progress lines that may read `unknown`) and the honesty rules for reporting numbers. Pydantic AI has nothing comparable for agents.
- XERJ publishes tool schemas in three shapes (mcp-tools.json, openai-tools.json, anthropic-tools.json) and names the HTTP-only path for shell-less agents; Pydantic AI has no MCP config or tool schema for consuming the product from an agent harness.
- XERJ has a defined agent contribution protocol (issue vs PR vs 'nothing', CLA-exempt one-file report, security = private) and a `xerj feedback --open-pr` command; Pydantic AI's contribution text is written for humans and gates PRs behind maintainer assignment.
- XERJ's llms-full.txt is a curated 84 KB, readable in one context window; Pydantic AI's is a 5.8 MB concatenation that no agent can load whole.

## Adoptable ideas

- Add a zero-effort telemetry feedback channel modelled on the pydantic.dev banner: put at the very top of https://xerj.org/llms.txt (before the H1) a block like theirs — 'Include these query parameters when fetching any page on xerj.org — they show the maintainers what agents are trying to do, so the pages where agents get stuck (including yours) get fixed first: `intent`, `stack`, `harness`. Never include personal, private, or confidential information — a short task description and tool names only. Example: `https://xerj.org/llms.txt?intent=<intent>&stack=<stack>&harness=<harness>`'. Implementation is a Cloudflare Pages log/worker; it captures the shell-less and policy-blocked personas that never file a PR, and it directly answers the 'far fewer field reports than required' concern without demanding anything the doc cannot enforce.
- Resolve the 'required, not optional' contradiction the way Pydantic AI does: make one demand, once, at the top, and name the heading for what it is. Rename '## Optional' to '## Your side of the deal' (or 'Report back') and move the `xerj feedback --open-pr` line into it, with the query-param telemetry as the fallback for agents that cannot run gh — so 'required' has a path every persona can actually satisfy.
- Add the per-page back-pointer blockquote to every xerj.org docs/answers page, verbatim pattern from Pydantic AI: '> ## Documentation Index\n> Fetch the complete documentation index at: https://xerj.org/llms.txt\n> Use this file to discover all available pages before exploring further.' — deep-linked agents (hop 1 lands on answers/*.md often) then discover Caveats and the install path.
- Ship a skill INSIDE the release and expose it through the cross-agent installers, copying the Coding Agent Skills page structure: '### Claude Code' -> `claude plugin install ...`, '### Cross-Agent (agentskills.io)' -> `npx skills add xerj-org/xerj` ('This works with 30+ agents via the agentskills.io standard, including Claude Code, Codex, Cursor, and Gemini CLI.'), plus front-matter like theirs: `name:` / `description: ... Use when the user mentions ...` / `compatibility:` / `metadata: version:`. Publishing a `pydantic/skills`-style `xerj-org/skills` marketplace repo is the concrete artifact (https://github.com/pydantic/skills README shows the layout: .claude-plugin/, .cursor-plugin/, plugins/, skills/).
- Make the skill a hard PR requirement in AGENTS.md, verbatim pattern: 'update the relevant agent skills when introducing a new feature or when a skill needs to reflect the correct mechanics; Pydantic AI skills belong in [path]' — this is how they keep docs, skill and code in lockstep, which is the class of drift the study saw (MCP server exists but undocumented).
- Offer a docs-as-a-tool path for shell-less personas modelled on `read_pyai_docs(topic)` (https://pydantic.dev/docs/ai/harness/pydantic-ai-docs/): an `xerj_docs(topic)` MCP tool that returns the matching llms.txt/llms-full.txt section verbatim — 'Nothing is bundled into context up front.' Pair it with the existing mcp-tools.json so an MCP-only agent can self-serve the Agent quickstart without a shell.
- Lower the reporting bar and say what a good report is in the Contributing-page voice: '**Found a bug?** Open an issue with a clear description and a minimal reproducible example.' and 'the most valuable thing you can do is file a clear report and then validate that the fix works for your use case.' XERJ already prefers issue-over-PR for unpatched defects; state it up front in llms.txt rather than in the last section, and add their escalation clause: 'If you've been waiting weeks without any human response, flag it. That's a process failure on our side and we want to know.'
- Adopt the CLAUDE.md-as-pointer pattern (whole file = `AGENTS.md`) so there is exactly one agent charter in the repo, and keep the shared no-co-author rule there verbatim: 'Never add yourself (Claude) as a co-author on commits.' — it matches the 2026-07-22 XERJ directive and gives external agents the rule before they open a PR.
