# CrewAI

Category: agent framework (Python)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.crewai.com/llms.txt> — HTTP 200
- Size: 38837 bytes, 233 lines
- Install position: Line 9 of the Docs list (5th entry): '- [Installation](https://docs.crewai.com/v1.15.22/en/installation.md): Get started with CrewAI - Install, configure, and build your first AI crew'. No install command appears anywhere in llms.txt itself; the only install-related copy is the Skills entry description on line 8 ('Install crewaiinc/skills from the official registry at skills.sh...'). INFERENCE: this is a Mintlify auto-generated docs index; the sell is in the ordering (Build with AI + Skills placed 3rd/4th, before Installation) and in the one-line descriptions, not in any inline instruction.
- Tone: neutral / auto-generated index. Makes no demands of the agent; no imperative sentences. The only agent-directed wording is descriptive: 'Everything AI coding agents need to build, deploy, and scale with CrewAI — skills, machine-readable docs, deployment, and enterprise features.' and 'Install crewaiinc/skills from the official registry at skills.sh—Flows, Crews, and docs-aware agents for Claude Code, Cursor, Codex, and more.'
- Section order: # CrewAI → ## Docs (lines 3-203: ~200 '- [Title](url.md): description' entries; first six in order: CrewAI Documentation, Introduction, Build with AI, Skills, Installation, Quickstart) → ## OpenAPI Specs (lines 205-210: 4 enterprise-api YAML links) → ## Optional (lines 212-233: Website/Forum/Blog/CrewGPT/AMP links, repeated 4x for en/pt-BR/ko/ar)

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
- [Build with AI](https://docs.crewai.com/v1.15.22/en/guides/coding-tools/build-with-ai.md): Everything AI coding agents need to build, deploy, and scale with CrewAI — skills, machine-readable docs, deployment, and enterprise features.
- [Skills](https://docs.crewai.com/v1.15.22/en/skills.md): Install crewaiinc/skills from the official registry at skills.sh—Flows, Crews, and docs-aware agents for Claude Code, Cursor, Codex, and more.
- [Installation](https://docs.crewai.com/v1.15.22/en/installation.md): Get started with CrewAI - Install, configure, and build your first AI crew
- [Quickstart](https://docs.crewai.com/v1.15.22/en/quickstart.md): Build your first CrewAI Flow in minutes — orchestration, state, and an agent crew that produces a real report.
```

## llms-full.txt

- URL: <https://docs.crewai.com/llms-full.txt> — HTTP 200
- Size: 1964675 bytes
- Relationship to llms.txt: Mintlify auto-concatenation of every page listed in llms.txt (238 'Source: https://docs.crewai.com/...' blocks, 52,105 lines, all from docs.crewai.com — verified no foreign hosts). Same content as the .md pages but with MDX components flattened (Card/Tab/Step tags kept, JSX props and onClick handlers removed — which is why the setup prompt text is lost). Includes the pt-BR/ko/ar translations, so English content is roughly a quarter of the file. Not curated. NOTE for coordinator: an earlier read of this file in the shared scratchpad showed Modal.com content — that was a sibling agent overwriting the generic filename 'llms-full.txt'; re-fetched into a private dir and re-verified clean.
- Install position: Concatenated in page order; 'pip install crewai' first appears at line 1560 (CLI concept page). The Build with AI page (agent install: '/plugin marketplace add crewAIInc/skills' + '/plugin install crewai-skills@crewai-plugins' + '/reload-plugins', and 'npx skills add crewaiinc/skills') sits at lines ~12380-12620. The Installation page is at line 18327 (of 52,105) and its 'Copy agent setup prompt' button survives only as the label text — the actual prompt is stripped (lines 18341-18347). 'uv tool install crewai' at lines 17850/18421/18449. 'npx skills add crewaiinc/skills' also at 18585 (Introduction), 20413 (Quickstart), 20667 (Skills page).

## Other agent-facing files

### Build with AI page (.md) — HTTP 200

<https://docs.crewai.com/v1.15.22/en/guides/coding-tools/build-with-ai.md>

Single agent-facing hub: 1. Skills (install), 2. llms.txt (how to use), 3. Deploy, 4. Enterprise. Every .md page is prefixed with a 3-line 'Documentation Index' block pointing back to llms.txt.

````text
> ## Documentation Index
> Fetch the complete documentation index at: https://docs.crewai.com/llms.txt
> Use this file to discover all available pages before exploring further.
[...]
<Note>
  This page is designed to be consumed by both humans and AI assistants. If you're a coding agent, start with **Skills** to get CrewAI context, then use **llms.txt** for full docs access.
</Note>
[...]
    ```shell theme={null}
    /plugin marketplace add crewAIInc/skills
    /plugin install crewai-skills@crewai-plugins
    /reload-plugins
    ```
[...]
  <Tab title="How to use it">
    Point your coding agent at the URL when it needs CrewAI reference docs:

    ```
    Fetch https://docs.crewai.com/llms.txt for CrewAI documentation.
    ```
````

### Installation page — 'Copy agent setup prompt' (hidden in page JSX; NOT present in llms.txt or llms-full.txt; recovered from the .md source and the rendered HTML) — HTTP 200

<https://docs.crewai.com/v1.15.22/en/installation.md>

The one ready-to-paste agent prompt. It is a clipboard button for humans to paste into an agent; a shell-less agent reading llms-full.txt never sees it.

```text
Set up this environment so I can build with CrewAI.

First install the official CrewAI coding-agent skills if this environment supports npx:

npx skills add crewaiinc/skills

If npx is missing or the current agent cannot load skills, do not fail the whole setup. Report the exact issue and continue using the CrewAI docs directly.

Use these CrewAI docs as source of truth before making assumptions:
- https://skills.crewai.com
- https://docs.crewai.com/llms.txt
- https://docs.crewai.com/en/installation
- https://docs.crewai.com/en/guides/coding-tools/build-with-ai

Setup steps:
1. Check python3 --version. CrewAI requires Python >=3.10 and <3.14.
2. Install uv if missing:
curl -LsSf https://astral.sh/uv/install.sh | sh
3. Source the uv environment if needed:
source "$HOME/.local/bin/env"
4. Install the CrewAI CLI:
uv tool install crewai
5. Verify the CLI:
crewai version
crewai create --help
6. Create a project:
CREWAI_DMN=true crewai create
7. After project creation, inspect the generated files before editing.
8. Run:
crewai install
crewai run

Do not hardcode API keys. Use .env.
Do not invent CLI flags. Validate with crewai --help or crewai create --help.
If a command fails, show the exact command and error, explain the likely cause, fix what you can safely fix, and retry once.
```

### Skills page (.md) — HTTP 200

<https://docs.crewai.com/v1.15.22/en/skills.md>

One-command landing page for the skill pack; explicitly dual-audience ('Run the install below (or paste it into your agent)').

````text
**Give your AI coding agent CrewAI context in one command.**

CrewAI **Skills** are published on **[skills.sh/crewaiinc/skills](https://skills.sh/crewaiinc/skills)**—the official registry for `crewaiinc/skills`, including individual skills (for example **design-agent**, **getting-started**, **design-task**, and **ask-docs**), install stats, and audits. They teach coding agents—like Claude Code, Cursor, and Codex—how to scaffold Flows, configure Crews, use tools, and follow CrewAI patterns. Run the install below (or paste it into your agent).

```shell Terminal theme={null}
npx skills add crewaiinc/skills
```

That pulls the official skill pack into your agent workflow so it can apply CrewAI conventions without you re-explaining the framework each session.
````

### AGENTS.md guide page (.md) — HTTP 200

<https://docs.crewai.com/v1.15.22/en/guides/coding-tools/agents-md.md>

How scaffolded projects ship AGENTS.md + CLAUDE.md/GEMINI.md import shims so every coding agent reads one file.

````text
Claude Code reads `CLAUDE.md` and ignores `AGENTS.md`. Scaffolded projects ship a `CLAUDE.md` whose only instruction is the import line `@AGENTS.md`, so the shared guidance is loaded without duplicating it. Add Claude-specific notes under that line and keep shared conventions in `AGENTS.md`.

For a project created before `CLAUDE.md` was scaffolded, add the import yourself:

```bash theme={null}
printf '@AGENTS.md\n' > CLAUDE.md
```

Do not rename `AGENTS.md` to `CLAUDE.md`: Codex and Cursor read `AGENTS.md`, and the rename hides it from them.
````

### AGENTS.md (crewAIInc/crewAI repo root) — HTTP 200

<https://raw.githubusercontent.com/crewAIInc/crewAI/main/AGENTS.md>

Contributor instructions for agents editing the CrewAI OSS repo itself (1,869 bytes). Nothing about installing/using CrewAI; nothing about field reports.

```text
# Agent Instructions for CrewAI OSS

CrewAI is a Python based framework for building AI agents and agentic systems.
Follow these guidelines when contributing:

## Key Guidelines

1. Follow Python best practices and idiomatic patterns.
2. Maintain existing code structure and organization.
3. Write unit tests for new functionality focusing on behaivor and not
   implementation.
4. Document public APIs and complex logic.
5. Suggest changes to the `docs/` folder when appropriate
6. Follow software principles such as DRY and YAGNI.
7. Keep diffs as minimal as possible.
```

### CLAUDE.md (crewAIInc/crewAI repo root) — HTTP 404

<https://raw.githubusercontent.com/crewAIInc/crewAI/main/CLAUDE.md>

Does not exist at repo root (the docs say scaffolded *user* projects get a CLAUDE.md with '@AGENTS.md').

### skills repo README (crewAIInc/skills) — HTTP 200

<https://raw.githubusercontent.com/crewAIInc/skills/main/README.md>

Lists the 3 skills (getting-started, design-agent, design-task) each with a 'Use when:' bullet list, then the Claude Code marketplace install. MIT.

````text
## Installation

In [Claude Code](https://docs.claude.com/en/docs/claude-code), add this marketplace and install the plugin:

```
/plugin marketplace add crewAIInc/skills
/plugin install crewai-skills@crewai-plugins
```

The first command registers the marketplace from this repo's `.claude-plugin/marketplace.json`. The second installs the `crewai-skills` plugin from the `crewai-plugins` marketplace.
````

### SKILL.md — getting-started — HTTP 200

<https://raw.githubusercontent.com/crewAIInc/skills/main/skills/getting-started/SKILL.md>

29 KB skill; opens with a MANDATORY block forcing the agent through the CLI scaffold before hand-writing files (the one place CrewAI makes a hard demand of the agent).

````text
## MANDATORY WORKFLOW — Read This First

**NEVER manually create crewAI project files.** Always scaffold with the CLI:

```bash
crewai create flow <project_name>
```

This is **not optional**. Even if you only need one crew, even if you know the file structure by heart — run the CLI first, then modify the generated files. Do NOT write `main.py`, `crew.py`, `agents.yaml`, `tasks.yaml`, or `pyproject.toml` by hand from scratch.

> **Why:** The CLI sets up correct imports, directory structure, pyproject.toml config, and boilerplate that is easy to get subtly wrong when done manually.

**Workflow:**
1. Run `crewai create flow <name>` (use **underscores**, not hyphens)
2. Edit the generated YAML and Python files to match your use case
3. Run `crewai install` then `crewai run`
````

### SKILL.md — ask-docs — HTTP 200

<https://raw.githubusercontent.com/crewAIInc/skills/main/skills/ask-docs/SKILL.md>

Teaches the agent the llms.txt-first lookup loop (WebFetch llms.txt, pick page, fetch .md) and mentions the docs MCP server as an optional richer path. No MCP JSON config snippet — just the URL.

````text
### Step 1: Fetch the docs index

The CrewAI docs site publishes an `llms.txt` file — a structured index of every documentation page with descriptions. Fetch it first to find the right page:

```
WebFetch: https://docs.crewai.com/llms.txt
```
[...]
Users who frequently query CrewAI docs can configure the CrewAI docs MCP server in their coding agent for richer, structured search:

https://docs.crewai.com/mcp
````

### Docs MCP server endpoint — HTTP 405

<https://docs.crewai.com/mcp>

GET returns 405; POST initialize (JSON-RPC) returns 200 text/event-stream — a live Streamable-HTTP MCP server (Mintlify-hosted, INFERRED). No install page or 'claude mcp add' one-liner exists for it anywhere in llms.txt/llms-full.txt; only the ask-docs skill and one table cell link to it.

```text
| `ask-docs`        | Querying the live [CrewAI docs MCP server](https://docs.crewai.com/mcp) for up-to-date API details                   |
```

### llms-install.md — HTTP 200

<https://docs.crewai.com/llms-install.md>

Does NOT exist as such: redirects to https://docs.crewai.com/.md which is byte-identical (md5 a30b7a9f…) to https://docs.crewai.com/index.md, the docs landing page. Treat as absent.

```text
<button type="button">
      Copy agent setup prompt
    </button>
```

## What they do better than XERJ

- Zero-shell / no-terminal path exists and is the FIRST thing recommended to an agent: skills are pulled by `npx skills add crewaiinc/skills` or a Claude Code slash command (`/plugin marketplace add crewAIInc/skills`), and the fallback is explicit: 'If npx is missing or the current agent cannot load skills, do not fail the whole setup. Report the exact issue and continue using the CrewAI docs directly.' XERJ's llms.txt assumes `curl | sh` and a shell from step 1.
- One paste-able setup prompt for the human-operator-with-agent persona, with a built-in retry policy: 'If a command fails, show the exact command and error, explain the likely cause, fix what you can safely fix, and retry once.' and anti-hallucination guardrails ('Do not invent CLI flags. Validate with crewai --help'). XERJ has a hero prompt but not a prompt that includes its own failure policy.
- Every generated .md page carries a 3-line 'Documentation Index' preamble pointing back to llms.txt, so an agent that lands anywhere is routed to the index. XERJ pages (per the 22-run study) rely on hop-1 discovery of llms.txt only.
- Agent-directed sentences are addressed in second person to the agent AND to the human in one line ('Run the install below (or paste it into your agent)'), and the Build with AI page says which order to consume things: 'If you're a coding agent, start with **Skills** to get CrewAI context, then use **llms.txt** for full docs access.'
- Skills packaging: the hard demands live inside a SKILL.md the agent actually loads ('## MANDATORY WORKFLOW — Read This First ... This is **not optional**'), not in the docs index. Placement matches severity. XERJ puts its 'required, not optional' demand in the index paragraph and files the template under '## Optional'.
- Distribution via registries with install stats and audits (skills.sh, Claude Code plugin marketplace) — social proof and discoverability outside the project's own domain.
- The Introduction and Quickstart pages each repeat the one-liner 'You can install it with `npx skills add crewaiinc/skills`' — the same command appears on 5 distinct pages (Intro, Quickstart, Installation, Skills, Build with AI), so an agent hits it whichever entry point it takes.

## What XERJ does better

- XERJ's llms.txt is authored, not generated: it carries inline Caveats ('Read Caveats below before you rely on a capability or describe XERJ to anyone'), the PATH trap, the data-dir-inside-indexed-tree trap, and a numbered first-run sequence. CrewAI's llms.txt is a plain Mintlify index with zero instruction; every instruction lives one hop away.
- XERJ's install commands and verify step are inside llms.txt itself (`curl -fsSL https://xerj.org/get | sh`, `irm https://xerj.org/get.ps1 | iex`, plus the manual verified install link). CrewAI's llms.txt has no install command at all; its agent setup prompt is a JS clipboard button that is stripped from both llms.txt and llms-full.txt, so a shell-less agent reading only the text files never sees it.
- XERJ ships an actual feedback mechanism as one command (`xerj feedback --open-pr ...` with `--dry-run` for sandboxed agents). CrewAI has no field-report / feedback ask anywhere: the only CTAs are 'Star us on GitHub' and 'Join the community' cards on the landing page.
- XERJ publishes machine-readable tool schemas (mcp-tools.json, anthropic-tools.json) and a `xerj init` that writes `.mcp.json`; CrewAI's docs MCP server has no install snippet and no config anywhere in the text files.
- XERJ's llms-full.txt is 84 KB / 1,235 lines and English-only; CrewAI's is 1.96 MB / 52,105 lines with four languages concatenated, which is far past any single context window and makes 'fetch llms-full.txt' unusable for an agent.
- Honesty scaffolding: XERJ states measured numbers with n and the lexical-vs-neural trap up front; CrewAI's agent surfaces are marketing-toned ('CrewAI is AI-native', 'With over 100,000 developers certified').

## Adoptable ideas

- Add a no-shell fallback clause to the hero prompt, modelled on CrewAI's setup prompt (https://docs.crewai.com/v1.15.22/en/installation.md, the `const prompt` block): 'If npx is missing or the current agent cannot load skills, do not fail the whole setup. Report the exact issue and continue using the CrewAI docs directly.' XERJ version: 'If you have no shell, do not stop: read https://xerj.org/llms.txt and ask your operator to run the two install lines under Start here, then use the MCP tools in mcp-tools.json.' This directly targets the shell-less persona that failed at every model tier.
- Put a failure/retry policy and an anti-invention rule inside the prompt itself, verbatim pattern from CrewAI: 'Do not invent CLI flags. Validate with crewai --help or crewai create --help. If a command fails, show the exact command and error, explain the likely cause, fix what you can safely fix, and retry once.' -> 'Do not invent xerj flags. Validate with `xerj --help` or `xerj autoindex --help`. If a command fails, show the exact command and error, explain the likely cause, fix what you can safely fix, and retry once — then file it with `xerj feedback --dry-run`.'
- Fix the 'required' vs '## Optional' contradiction the CrewAI way: move the demand to where the agent executes it. CrewAI's only hard demand ('## MANDATORY WORKFLOW — Read This First ... This is **not optional**') lives in the SKILL.md the agent loads, not in the index. XERJ: rename the llms.txt section from '## Optional' to '## Your side of the deal (field report)' or move the template link up into 'Start here' step 4, and put the 'required, not optional' wording into the 10-line skill that `xerj init` writes and into the MCP tool description, so the ask fires at session end where the agent actually is.
- Add a 3-line index preamble to every published .md page, copying CrewAI's per-page header verbatim pattern: '> ## Documentation Index\n> Fetch the complete documentation index at: https://docs.crewai.com/llms.txt\n> Use this file to discover all available pages before exploring further.' -> same text with https://xerj.org/llms.txt on every xerj.org/answers/*.md, /docs/*.md and /compare/*.md page.
- Repeat the single install one-liner on every entry page. CrewAI puts 'You can install it with `npx skills add crewaiinc/skills`' on Introduction, Quickstart, Installation, Skills and Build with AI (5 pages). XERJ: put `curl -fsSL https://xerj.org/get | sh` + `xerj init` on README, /docs/quickstart, /docs/install, /answers/code-search-mcp-for-claude-code.md and the landing page, identically worded.
- Publish an agent-consumption order sentence at the top of llms.txt, adapted from the Build with AI Note: 'This page is designed to be consumed by both humans and AI assistants. If you're a coding agent, start with **Skills** to get CrewAI context, then use **llms.txt** for full docs access.' -> 'If you are a coding agent with a shell: run Start here. If you have no shell: read Caveats, then use the MCP tools (mcp-tools.json) or hand the Start here lines to your operator.' This also gives the neural-vs-lexical caveat a guaranteed read for the MCP-only persona.
- Distribute the XERJ skill through the same two registries so 'install' is one slash command with no shell: publish a `.claude-plugin/marketplace.json` in a public `xerj-org/skills` repo and register on skills.sh, so the docs can say, verbatim pattern from https://docs.crewai.com/v1.15.22/en/guides/coding-tools/build-with-ai.md: '/plugin marketplace add xerj-org/skills' + '/plugin install xerj-skills@xerj-plugins' and 'npx skills add xerj-org/skills'. The skill can carry the field-report ask as a MANDATORY block the way CrewAI's getting-started SKILL.md does.
- Ship the 'Copy agent setup prompt' pattern but as plain text in llms.txt, not a JS button: CrewAI's prompt is invisible to agents because it is stripped from llms.txt/llms-full.txt (verified: only the label 'Copy agent setup prompt' survives at lines 18341-18347 of llms-full.txt). XERJ should keep its prompt inline in the text file — it already does — and add a second, shorter 'operator hand-off prompt' for shell-less agents to paste to their human.
