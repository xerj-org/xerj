# Composio

Category: agent tooling (tool/auth platform)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.composio.dev/llms.txt> — HTTP 200
- Size: 4445 bytes, 50 lines
- Install position: Line 11, inside '## Before you implement' (section 1 of 6, before any link list). The ONLY install command in the file is the skill install; product install (CLI/SDK) is NOT in llms.txt — it is delegated to the linked .md pages. Line 20 mentions `composio login --agent` inline in a link description.
- Tone: imperative, second-person agent procedure. It makes explicit demands of the agent: 'Inspect', 'Explain where Composio fits', 'ask what the user wants ... before making changes', 'Load and follow', 'requesting approval if your host requires it', 'report that fallback', 'clarify that first'. No marketing adjectives beyond the one-line tagline ('1,500+ toolkits'). Not neutral: it routes the agent through a decision tree (Platform vs For You; integrate-into-app vs connect-to-coding-agent) before any link.
- Section order: # Composio Documentation (H1 + blockquote tagline + 1 routing paragraph) → ## Before you implement → ## Choose your path → ## Build an application → ## API Reference (v3.1, current) → ## API Reference (v3.0, legacy) → ## Optional

### Install commands found

```sh
npx skills add ComposioHQ/composio --skill composio
composio login --agent (mentioned in a link description on line 20, not as a code block)
```

### Text written for an agent (verbatim)

```text
## Before you implement

Inspect the project's framework, agent architecture, authentication, and user or tenant identity model. Explain where Composio fits. If the intended workflow is unclear, ask what the user wants their application or agent to accomplish with connected apps before making changes.

Load and follow the official [Composio Agent Skill](https://github.com/ComposioHQ/composio/blob/next/skills/composio/SKILL.md) for setup guidance and implementation patterns. If it is missing, install it for the project with `npx skills add ComposioHQ/composio --skill composio`, requesting approval if your host requires it. If skills are unsupported or installation fails, use the documentation linked below and report that fallback.

If it is unclear whether the goal is to integrate Composio into the application or connect apps to the coding agent itself, clarify that first. Fetch the relevant Markdown pages below for the chosen path. Use the complete index when you need a guide or reference not listed here.
```

### MCP snippet (verbatim)

````text
(none in llms.txt) — from https://docs.composio.dev/docs/composio-connect.md, '## Generic MCP URL':
```json
{
  "mcpServers": {
    "composio": {
      "url": "https://connect.composio.dev/mcp",
      "headers": {
        "x-consumer-api-key": "YOUR_CONSUMER_KEY"
      }
    }
  }
}
```
````

### Feedback / contribution ask (verbatim)

```text
(none in llms.txt — no feedback, contribution, issue or PR ask anywhere in the 50 lines). The only support CTA in the docs corpus is on composio-connect.md '### I still need help': "Reach out at [support@composio.dev](mailto:support@composio.dev) or join the [Composio Discord](https://discord.com/invite/cNruWaAhQk)."
```

### Best passage for driving install and first use (verbatim)

```text
Load and follow the official [Composio Agent Skill](https://github.com/ComposioHQ/composio/blob/next/skills/composio/SKILL.md) for setup guidance and implementation patterns. If it is missing, install it for the project with `npx skills add ComposioHQ/composio --skill composio`, requesting approval if your host requires it. If skills are unsupported or installation fails, use the documentation linked below and report that fallback.

If it is unclear whether the goal is to integrate Composio into the application or connect apps to the coding agent itself, clarify that first. Fetch the relevant Markdown pages below for the chosen path. Use the complete index when you need a guide or reference not listed here.

## Choose your path

- [Platform or For You](https://docs.composio.dev/docs.md): Choose between building an application and using your own connected apps.
- [Set up a coding agent](https://docs.composio.dev/docs/agent-setup.md): Install the Composio skill to add Composio to an existing project.
- [SDK quickstart](https://docs.composio.dev/docs/quickstart.md): Install Python or TypeScript packages, create a session, and run an agent.
- [Authenticate an unattended agent](https://docs.composio.dev/docs/agent-setup/unattended-authentication.md): When no human is available, use `composio login --agent`, configure a project API key, and verify a live tool call. Human account access still requires authorization.
- [Native agent plugins](https://docs.composio.dev/docs/agent-plugins.md): Use your own apps from Codex or Claude Code.
- [Connect an MCP client](https://docs.composio.dev/docs/composio-connect.md): Connect an existing client to your apps over MCP.
```

### One-click links

- `(none in llms.txt itself) — elsewhere in the docs: windsurf://windsurf-mcp-registry?serverName=composio (composio-connect.md line 161); https://github.com/mcp/ComposioHQ/composio 'Install in VS Code' (composio-connect.md line 155); https://cursor.com/marketplace/composio (composio-connect.md); Claude Code: /plugin marketplace add ComposioHQ/composio-plugin-cc + /plugin install composio@composio (agent-plugins.md); Codex: codex plugin marketplace add https://github.com/ComposioHQ/composio-plugin-openai.git --json + codex plugin add composio@composio --json`

## llms-full.txt

- URL: <https://docs.composio.dev/llms-full.txt> — HTTP 200
- Size: 1160265 bytes
- Relationship to llms.txt: Concatenated docs: 248 page bodies (`# Title (/path)` headings, `---` separators), 1771 headings total, generated from the same Fumadocs .md pages that llms.txt links to. NOT curated and NOT the same content as llms.txt: llms-full.txt does not contain the '## Before you implement' agent procedure at all (grep confirms it is absent); its preamble is instead a separate 'Composio SDK — Notes for AI Code Generators' block (lines 7-103) that is ALSO appended verbatim to the foot of every individual .md page (seen on agent-setup.md lines 39-132, clients.md line 246+, composio-connect.md line 291+). There is a third file, https://docs.composio.dev/llms-index.txt (200, 45,940 bytes), which is the complete link index and says 'For a shorter routing map, start at [llms.txt]'. So the trio is: llms.txt = 50-line agent router, llms-index.txt = full link index, llms-full.txt = full page bodies.
- Install position: Product install first appears at line 147 (`npx skills add ...` inside the Quickstart page's embedded agent prompt) and lines 170-186 (`uv add` / `pip install` / `npm install` per framework). CLI install `curl -fsSL https://composio.dev/install | sh` at line 3252 (Agent plugins page) and line 3419 (CLI page). MCP JSON at ~line 4018 (Composio Connect page). Pages are ordered by the docs sidebar: Notes for AI Code Generators (line 7) -> /docs overview (109) -> Quickstart (135) -> providers (676-3245) -> Agent plugins (3246) -> Using the skill (3338) -> CLI (3404) -> Composio Connect (3850) -> agent-setup (4136) -> unattended auth (4170) -> clients (4252) -> concepts/auth/etc. -> KB.

## Other agent-facing files

### agent-setup.md (9-client page) — HTTP 200

<https://docs.composio.dev/docs/agent-setup.md>

One-page hub: install the skill with one command, choose your coding agent, then hand the agent a prompt. Lists 9 clients with per-client anchors. Every .md page carries the same footer pointing back to llms.txt and the same appended 'Notes for AI Code Generators' block (terminology migration table, common generated-code mistakes).

````text
Run the Skills CLI from your project directory:

```bash
npx skills add ComposioHQ/composio --skill composio
```

Choose your coding agent when prompted. See [Clients](/docs/agent-setup/clients) for instructions for Claude Code, Codex, Cursor, GitHub Copilot, Gemini CLI, OpenClaw, OpenCode, Cline, and Grok Build.

After installation, ask your agent to add Composio. It should inspect your framework, agent architecture, authentication, and user identity model before recommending an integration.

[...footer on every .md:]
📚 **More documentation:** [View all docs](https://docs.composio.dev/llms.txt) | [Changelog](https://docs.composio.dev/docs/changelog.md) | [Glossary](https://docs.composio.dev/llms.mdx/reference/glossary) | [Examples](https://docs.composio.dev/llms.mdx/examples) | [API Reference](https://docs.composio.dev/llms.mdx/reference)
````

### SKILL.md (the skill llms.txt points at) — HTTP 200

<https://raw.githubusercontent.com/ComposioHQ/composio/next/skills/composio/SKILL.md>

86-line router skill: choose product (table), choose job (5 verbs), load only the relevant reference, 11 'Stable rules', and a canonical-sources block that tells the agent to fetch llms.txt / <page>.md / a knowledge-search API. Frontmatter description is written as a trigger list for skill-aware hosts.

````text
Use this skill as a router. Identify the product and the job, load only the relevant guidance, consult canonical documentation for volatile details, and then answer or do the work the user requested.
[...]
Do not turn an explanation, documentation lookup, or narrow bug fix into onboarding.
[...]
10. Do not invent repository facts. Never claim that a file, framework, environment loader, identity field, agent path, or dependency exists until it was provided or inspected. If codebase context is unavailable, state the unknown and ask for access or one necessary detail.
[...]
```text
https://docs.composio.dev/llms.txt
https://docs.composio.dev/docs/<page>.md
https://docs.composio.dev/toolkits/<toolkit>.md
```

If those primary sources do not answer a Composio product or troubleshooting question, query the public unified knowledge search at `https://docs.composio.dev/api/knowledge-search?q=<question>`.
[...]
Use the documentation to complete the task. Do not merely hand the user a link unless they asked for one.
````

### unattended-authentication.md (no-human path) — HTTP 200

<https://docs.composio.dev/docs/agent-setup/unattended-authentication.md>

Install -> login as an agent -> extract key without printing it -> verify with a real tool call whose success criteria are spelled out -> what to report if only the smoke check passed. Includes explicit anti-abuse constraints and the human fallback (--no-wait / --poll).

````text
```bash
curl -fsSL https://composio.dev/install | sh
export PATH="$HOME/.local/bin:$PATH"
composio login --agent
composio agent whoami
```
[...]
Check both the HTTP result and the response body. Require `successful: true`, no tool error, and profile data for the requested username. An HTTP 401, a tool schema lookup, or a successful build does not prove tool execution. A local mock verifies only the local code path.
[...]
Record the tool slug, sanitized result, and returned log ID if present. If only the Hacker News check succeeds, report that check separately from the requested integration action. If app authorization is unavailable, report the remaining connection step and leave the action unverified.
[...]
* Use the supported CLI flow. Do not reverse-engineer browser signup, create disposable inboxes, or bypass browser challenges to obtain a Composio key.
````

### clients.md (per-agent install + paste-prompt) — HTTP 200

<https://docs.composio.dev/docs/agent-setup/clients.md>

For each of 9 agents: where that agent loads skills from, a global and a project install command with --agent <name>, 'start a new session', and a 3-line prompt the human pastes to the agent.

````text
**Global install**

```bash
npx skills add ComposioHQ/composio --skill composio --agent claude-code --global
```

**Project install**

```bash
npx skills add ComposioHQ/composio --skill composio --agent claude-code
```

The Skills CLI installs the skill in Claude Code's skill directory. Start a new Claude Code session if the current session does not discover it, then ask Claude to add Composio to your project.

```text
Use the /composio skill to get Composio working in this codebase.

Help me connect an integration and make my first real tool call.
When it works, show me what changed and what I can try next.
```
````

### composio-connect.md (MCP install page, 17 clients) — HTTP 200

<https://docs.composio.dev/docs/composio-connect.md>

Per-client MCP install: Claude Code, Claude Desktop, ChatGPT, Cursor marketplace, OpenClaw, Codex, Warp, Grok, Gemini CLI, VS Code (GitHub MCP registry), Windsurf deep link, Antigravity, Agent Builder, n8n, generic JSON. For CLI-capable agents the 'install' is a one-line prompt to paste; for GUI clients it is video-backed clicks.

````text
## Claude Code

#### Ask Claude Code to install Composio

Paste this prompt into Claude Code:

```
Install the Composio CLI: curl -fsSL https://composio.dev/install | sh, then run composio login.
```
[...]
Open `windsurf://windsurf-mcp-registry?serverName=composio` in Devin Desktop to install Composio.
````

### agent-plugins.md (native Claude Code / Codex plugin) — HTTP 200

<https://docs.composio.dev/docs/agent-plugins.md>

CLI install -> `composio setup --target auto` detects and configures every supported agent host; `--yes` flag for non-interactive/agent runs; plugin marketplace commands for Claude Code and Codex.

````text
```bash
composio login
composio setup --target auto
```

`auto` detects Codex and Claude Code. If both are installed, it configures both.

> **Running setup from an agent or script?**: Setup asks before changing local files. Add `--yes` in a non-interactive shell: `composio setup --target auto --yes`.
[...]
```bash
/plugin marketplace add ComposioHQ/composio-plugin-cc
/plugin install composio@composio
```
````

### AGENTS.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/ComposioHQ/composio/next/AGENTS.md>

Contributor-facing repo rules (4,992 bytes): first steps, skill routing table to ~20 local skills, repo map, generated/vendored paths not to edit, common commands. Not a product-install document.

```text
1. Read the nearest nested `AGENTS.md` before editing a subtree.
2. Preserve unrelated dirty work. Do not revert, delete, or reformat files outside the requested scope.
3. Treat `.agents/skills` as the canonical local skill tree. `.claude/skills` is a compatibility symlink and must not be edited as a separate copy.
```

### CLAUDE.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/ComposioHQ/composio/next/CLAUDE.md>

199-byte shim redirecting to AGENTS.md.

```text
Claude Code compatibility shim.

Use `AGENTS.md` for repository guidance. Local skills are canonical under `.agents/skills`; `.claude/skills` points there for Claude-compatible clients.
```

### llms-install.md — HTTP 404

<https://docs.composio.dev/llms-install.md>

does not exist

### llms-index.txt (complete link index) — HTTP 200

<https://docs.composio.dev/llms-index.txt>

45,940-byte full docs index; llms.txt delegates to it via 'Complete documentation index' link under Optional.

```text
For a shorter routing map, start at [llms.txt](https://docs.composio.dev/llms.txt).
```

## What they do better than XERJ

- Length and shape: 50 lines / 4.4 KB written as a procedure an agent executes top-to-bottom, versus XERJ's 234 lines / 40 KB index. Composio moves everything non-routing to llms-index.txt (45 KB) and llms-full.txt (1.1 MB) — three files with explicit roles, and llms-index.txt says 'For a shorter routing map, start at llms.txt'.
- A graceful-degradation ladder for hosts without a shell or without skills, stated in one sentence: 'If it is missing, install it ... requesting approval if your host requires it. If skills are unsupported or installation fails, use the documentation linked below and report that fallback.' This is exactly the shell-less/MCP-only persona XERJ's study found failing at every tier.
- 'Ask before acting' is built in: 'If the intended workflow is unclear, ask what the user wants ... before making changes' and 'If it is unclear whether the goal is to integrate Composio into the application or connect apps to the coding agent itself, clarify that first.' XERJ's llms.txt has no equivalent decision gate; it assumes the agent is installing for itself.
- Verification is defined as a real call with spelled-out pass criteria and an explicit list of what does NOT count: 'An HTTP 401, a tool schema lookup, or a successful build does not prove tool execution. A local mock verifies only the local code path.' XERJ's first-run list has no verification step.
- Per-client install matrix with the exact paste-prompt for each of 9 coding agents and 17 MCP clients, including one-click/registry links (windsurf://, GitHub MCP registry, Cursor marketplace, Claude Code /plugin marketplace add). XERJ documents `xerj init` for Claude Code/Cursor only and has no per-client page.
- Every generated .md page carries a footer back to llms.txt, so an agent that lands on any deep page can find the router.
- Agent-mode flags on the tooling itself: `composio login --agent` (no human, no browser), `composio setup --target auto --yes` (non-interactive), `composio login --no-wait` / `--poll` (human present but no browser). XERJ has `xerj feedback --dry-run` but no documented no-human/no-browser install-and-verify path.
- Skill has an explicit anti-overreach rule XERJ lacks: 'Do not turn an explanation, documentation lookup, or narrow bug fix into onboarding.' and 'Do not invent repository facts.'
- Reported by the coordinator (not verified by me): llms.txt has been rewritten as agent-first since the first Composio docs; the 'Notes for AI Code Generators' block appended to every page targets the specific mistakes seen in generated code (wrong provider package, legacy terminology).

## What XERJ does better

- Composio's llms.txt contains NO feedback, contribution, issue or PR ask of any kind — zero occurrences of feedback/contribute/issue/pull request in the 50 lines; the only support CTA in the whole docs corpus is 'support@composio.dev or join the Composio Discord'. XERJ at least has a field-report mechanism with a one-command path (`xerj feedback --open-pr`) and a CLA-exempt one-file PR contract.
- XERJ's Caveats section is inline and honest about the lexical-vs-neural trap; Composio's llms.txt has no caveats/limitations section at all.
- XERJ documents the PATH consequence of a non-profile-editing installer and the data-dir-inside-indexed-folder trap explicitly; Composio's llms.txt has no install gotchas (they live only on the CLI page).
- XERJ's llms.txt is self-contained enough to run the product end-to-end (install, start node, autoindex, query) without following a link; Composio's llms.txt cannot install the product without following at least one link — the product install is not in the file.
- XERJ ships machine-readable tool schemas (mcp-tools.json, anthropic-tools.json) linked from llms.txt; Composio's llms.txt links no schemas.
- XERJ's numbers are traced ('measured 2.7× fewer output tokens ... 16/16 tasks solved'); Composio's only number is the marketing '1,500+ toolkits'.

## Adoptable ideas

- Add a 'Before you implement' / 'Before you install' section as the FIRST H2, above 'Start here', containing the fallback ladder. Model verbatim from https://docs.composio.dev/llms.txt line 11: 'If it is missing, install it for the project with `...`, requesting approval if your host requires it. If skills are unsupported or installation fails, use the documentation linked below and report that fallback.' XERJ version: 'If you have a shell: `curl -fsSL https://xerj.org/get | sh`. If your host requires approval for installs, request it. If you have no shell, use the HTTP-only path at https://xerj.org/docs/agents/quickstart and say so in your field report.'
- Add a decision gate before install, per llms.txt line 13: 'If it is unclear whether the goal is to integrate Composio into the application or connect apps to the coding agent itself, clarify that first.' XERJ analogue: 'If it is unclear whether the goal is to index the user's data for them or to give yourself a reference-coding corpus, clarify that first.'
- Move the field-report ask OUT of '## Optional' (line 223 of XERJ's llms.txt) and into the numbered first-run list only, and rename the section. Composio's '## Optional' contains only changelog/index/KB links — nothing that is asked of the agent. The heading name contradicts 'required, not optional' on line 7; a coordinator can test whether renaming to e.g. '## Your side of the deal' changes PR arrival rate.
- Define verification with an explicit not-a-pass list, per https://docs.composio.dev/docs/agent-setup/unattended-authentication.md: 'Check both the HTTP result and the response body. Require `successful: true`, no tool error, and profile data for the requested username. An HTTP 401, a tool schema lookup, or a successful build does not prove tool execution.' XERJ version: 'Require a `hits.total.value` > 0 on a query for a string you know is in the indexed folder. A green `/_cluster/health`, a `--dry-run`, or `xerj --version` does not prove indexing worked.' Then tie the field report to this: 'Record the query, the hit count and the version; that is the body of the field report.'
- Add a 'Running as an unattended agent?' callout like https://docs.composio.dev/docs/quickstart.md: '> **Running as an unattended coding agent?**: If no human is available to log in, use `composio login --agent` ...'. XERJ: '> **No `gh`, no human?** Run `xerj feedback --dry-run`, paste the printed report into your final message, and tell the operator the file path to commit. The obligation transfers rather than disappearing.' (XERJ already says this on line 24 but buried mid-item; make it a blockquote callout.)
- Add a per-client install page with a 3-line paste-prompt per agent, per https://docs.composio.dev/docs/agent-setup/clients.md: 'Use the /composio skill to get Composio working in this codebase.\n\nHelp me connect an integration and make my first real tool call.\nWhen it works, show me what changed and what I can try next.' XERJ: 'Install XERJ with https://xerj.org/get, index this folder, run one search that proves it works, then file the field report with `xerj feedback --open-pr`. Show me what changed.' Cover Claude Code, Codex, Cursor, Copilot, Gemini CLI, Cline, OpenCode at minimum, each with `xerj init --agent <name>` if it exists, or the .mcp.json snippet if not.
- Split the file into three with stated roles, per llms-index.txt line 5: 'For a shorter routing map, start at [llms.txt]'. XERJ: keep a ~60-line llms.txt router (Before you install / Start here / Caveats / Choose your path / Your side of the deal), move the 100+ Answers/Comparisons links to llms-index.txt, and leave llms-full.txt as the deep reference.
- Add the footer to every generated .md/answers page, verbatim pattern from all Composio pages: '📚 **More documentation:** [View all docs](https://docs.composio.dev/llms.txt) | [Changelog](...) | ...'. XERJ: '**More:** [llms.txt](https://xerj.org/llms.txt) | [Caveats](https://xerj.org/llms.txt#caveats) | [File a field report](https://github.com/xerj-org/xerj/tree/main/user-feedback/16-agent-field-reports)' — this puts the report ask on every page an agent might land on from search, not only on the index.
