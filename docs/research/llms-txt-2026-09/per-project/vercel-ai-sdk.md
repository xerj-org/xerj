# Vercel AI SDK (+ vercel.com platform files)

Category: AI infra / SDK

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://ai-sdk.dev/llms.txt> — HTTP 200
- Size: 2217 bytes, 46 lines
- Install position: Lines 22-30, section '## Local Coding Agents' (second of four sections; the first section is '## Web Access' for agents that can fetch URLs). It is the only code block in the file. The SDK's own `npm install ai` does NOT appear in llms.txt at all - only the skill install `npx skills add vercel/ai`; package installation is delegated to the skill ('Then follow the skill instructions before changing code').
- Tone: Imperative but capability-conditional and terse: every instruction is gated on an 'If you can fetch URLs' / 'If you are working inside a local coding project with filesystem access' precondition. Two demands of the agent: 'search the docs first' and 'install the AI SDK skill first ... Then follow the skill instructions before changing code.' No marketing adjectives, no numbers, no obligations to report back. Zero contribution/feedback ask anywhere in llms.txt.
- Section order: # AI SDK (title + one-line blockquote + 1-line usage instruction) → ## Web Access → ## Local Coding Agents → ## Common Starting Points → ## Full Documentation

### Install commands found

```sh
npx skills add vercel/ai
```

### Text written for an agent (verbatim)

````text
Use this page to find current AI SDK documentation. Prefer search results and targeted Markdown pages over loading the full documentation bundle.

## Web Access

If you can fetch URLs, search the docs first:

- Search endpoint: https://ai-sdk.dev/api/search-docs?q=your+query

Examples:

- https://ai-sdk.dev/api/search-docs?q=building+agents
- https://ai-sdk.dev/api/search-docs?q=ToolLoopAgent
- https://ai-sdk.dev/api/search-docs?q=prepareStep
- https://ai-sdk.dev/api/search-docs?q=generating+structured+output

The search endpoint returns JSON with documentation URLs. Fetch the returned URLs with `.md` appended to get Markdown content.

## Local Coding Agents

If you are working inside a local coding project with filesystem access, install the AI SDK skill first:

```sh
npx skills add vercel/ai
```

Then follow the skill instructions before changing code.
````

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

````text
## Local Coding Agents

If you are working inside a local coding project with filesystem access, install the AI SDK skill first:

```sh
npx skills add vercel/ai
```

Then follow the skill instructions before changing code.
````

## llms-full.txt

- URL: <https://ai-sdk.dev/llms-full.txt> — HTTP 200
- Size: 6062390 bytes
- Relationship to llms.txt: Concatenated, machine-generated dump of the whole docs site (MDX with unrendered JSX components like <Snippet>, <IndexCards>, <QuickstartFrameworkCards />), NOT curated and NOT a superset of llms.txt's agent instructions - the Web Access / Local Coding Agents / skill guidance from llms.txt is absent. llms.txt explicitly de-prioritises it: 'Prefer search results and targeted Markdown pages over loading the full documentation bundle' and describes it as 'A concatenated Markdown copy of the AI SDK documentation for models with large context windows.' Feedback asks inside are page-level and product-specific, e.g. line 5677: 'Your feedback during this beta phase directly shapes the final stable release. Share your experiences through [GitHub issues](https://github.com/vercel/ai/issues/new/choose).'
- Install position: No agent-facing install section. The file literally starts with a raw TypeScript source file (line 1: '// CONTRIBUTING GUIDE', then 'export interface Tool {' - the tool-registry data file) before any docs page. The first SDK install command is an MDX snippet at line 775: '<Snippet text="pnpm add ai @ai-sdk/react @ai-sdk/openai" />' inside the RAG Chatbot Guide (H1 at line 526). 593 H1 headings; first H1s are guides ('# RAG Chatbot Guide', '# Multi-Modal Chatbot', ...) then '# AI SDK Core' at line 6187. `npx skills add` does not appear in llms-full.txt.

## Other agent-facing files

### vercel.com/llms.txt (platform index with '## How agents should use Vercel' + '## Agent setup') — HTTP 200

<https://vercel.com/llms.txt>

64-line curated platform index. Sections: When to use Vercel / How agents should use Vercel / Agent setup / Documentation / REST API and developer resources / Self-serve onboarding / Knowledge Base / Learning and updates / Optional. Agent setup gives four routes (agent playbook, human getting-started, plugin, skills) with the install command inline in the link description. Its '## Optional' section holds only machine catalogs (ai-catalog.json, taxonomy.json, graph.json) - nothing an agent is expected to act on, which is the semantic XERJ's 'Optional' heading currently violates.

```text
## How agents should use Vercel

- For research, fetch the Markdown documentation indexes below and follow their links to individual Markdown pages.
- For REST API calls, read the OpenAPI description and authentication documentation before choosing an operation. Ask for approval before changing account resources.
- For Vercel MCP, use an OAuth-capable MCP client and let the user authorize access to their Vercel account.

## Agent setup

- [Set up Vercel for your coding agent](https://vercel.com/get-started.md): Follow the agent playbook to install the CLI, add Vercel guidance, and connect Vercel MCP.
- [Getting started with Vercel](https://vercel.com/docs/getting-started-with-vercel.md?from=llms-txt): Install the Vercel CLI, add the Vercel plugin or Vercel Skills, and deploy your first project.
- [Vercel plugin](https://vercel.com/docs/agent-resources/vercel-plugin.md?from=llms-txt): Install with `npx plugins add vercel/vercel-plugin` for Vercel skills, commands, and specialist agents.
- [Vercel Skills](https://vercel.com/docs/agent-resources/skills.md?from=llms-txt): Browse and install individual skills for coding agents that do not support the plugin.
```

### vercel.com/get-started.md - 'Set up Vercel for your AI coding agent' agent playbook (235 lines, YAML front-matter with supported_surfaces/runtimes) — HTTP 200

<https://vercel.com/get-started.md>

A complete install prompt written TO the agent: check-before-install (`vercel --version` / `vercel whoami`), pause-only-for-auth rule, plugin-preferred/skills-fallback branching, per-runtime MCP routes (Claude Code, Codex CLI, Cursor, VS Code Copilot, Windsurf, Gemini CLI) each with the exact command or JSON to merge, a post-auth verification checklist (call `search_vercel_documentation`, then read-only `list_teams`), and a mandatory fixed-format completion report the agent must print with 'Report only verified state'. This is the closest thing to what XERJ's FTX study said was missing for shell-less/policy-constrained personas.

````text
Perform actions yourself when terminal or file access is available. Global or user-scoped installation is the default. Pause only for user authentication, approval, or UI actions you cannot perform. Do not merely return commands for the user to copy.

## 1. Install the Vercel CLI

Check for an existing installation:

```sh
vercel --version
vercel whoami
```

[...]

## Completion

Report only verified state:

```text
▲ Vercel agent setup is ready
CLI: <version>, authenticated as <username>
Guidance: <plugin|skills|skipped>, global or user scope
MCP: <connected|skipped>, global shared endpoint, https://mcp.vercel.com
MCP config: <path|managed by Vercel CLI>
Authenticated MCP check: <list_teams succeeded|not requested>
Reload: <not needed|completed>
```
````

### AGENTS.md on vercel/ai (320 lines, 13,420 bytes); CLAUDE.md is the 9-byte string 'AGENTS.md' — HTTP 200

<https://raw.githubusercontent.com/vercel/ai/main/AGENTS.md>

Repo-contributor guide for coding agents (not a user-facing install doc): monorepo layout table, dev setup, commands, coding standards, error pattern, ADRs, contributing-guide table, changesets, 'Task Completion Guidelines' per task type (bug fix / feature / refactor) with 'When to Deviate', and a closing '## Do Not' list. The contribution obligation is stated as a flat requirement tied to a mechanism that CI enforces ('Every PR modifying production code needs a changeset'), not a moral ask. CLAUDE.md (https://raw.githubusercontent.com/vercel/ai/main/CLAUDE.md, 200, 9 bytes) contains only 'AGENTS.md' - a pointer, single source of truth.

```text
## Changesets

- **Required**: Every PR modifying production code needs a changeset
- **Default**: Use `patch` (non-breaking changes)
- **Command**: `pnpm changeset` in workspace root
- **Note**: Don't select example packages - they're not published

## Task Completion Guidelines

These guidelines outline typical artifacts for different task types. Use judgment to adapt based on scope and context.

### Bug Fixes

A complete bug fix typically includes:

1. **Reproduction example**: Create/update an example in `examples/` that demonstrates the bug before fixing
2. **Unit tests**: Add tests that would fail without the fix (regression tests)
3. **Implementation**: Fix the bug
4. **Manual verification**: Run the reproduction example to confirm the fix
5. **Changeset**: Describe what was broken and how it's fixed
```

### skills/use-ai-sdk/SKILL.md - the skill that `npx skills add vercel/ai` installs (78 lines, 5,547 bytes; name: ai-sdk) — HTTP 200

<https://raw.githubusercontent.com/vercel/ai/main/skills/use-ai-sdk/SKILL.md>

The real install prompt. Package install is conditional and minimal ('If `node_modules/ai/` does not exist, install **only** the `ai` package'), it tells the agent to distrust its own memory, to read version-matched docs that ship inside node_modules, to fetch the model list live with curl rather than from memory, to check installed-vs-latest version (`npm view ai version`) and tell the user when stale, and to run the type checker after changes. Sections: What the AI SDK Is / Critical: Do Not Trust Your Own Memory / Use the Bundled, Version-Matched Docs / AI Gateway: The Fastest Way to Start / Choosing a Model / Building and Consuming Agents / DevTools / Keep the SDK Current / After Making Changes. No feedback/PR ask at all. (Note: ai-sdk.dev/llms-install.md returned HTTP 500; skills/ai-sdk/SKILL.md and skills/README.md returned 404 - the skill lives at skills/use-ai-sdk/.)

```text
## Critical: Do Not Trust Your Own Memory

Whatever you remember about the AI SDK is likely outdated. The SDK changes frequently across versions - APIs are renamed, removed, and added. Your training data almost certainly contains obsolete APIs, deprecated patterns, and model IDs that no longer exist. UI hooks like `useChat` are among the most frequently changed APIs, so be especially careful with client code.

**Never write AI SDK code from memory.** Always verify every API, option, and pattern against the documentation and source code for the version that is actually installed in the project.

## Use the Bundled, Version-Matched Docs

The `ai` package ships its full documentation and source code inside `node_modules`. These always match the installed version, so trust them over anything you remember.

1. Ensure `ai` is installed. If `node_modules/ai/` does not exist, install **only** the `ai` package using the project's package manager (e.g. `pnpm add ai`). Install provider packages (e.g. `@ai-sdk/openai`) and framework packages (e.g. `@ai-sdk/react`) later, when the task requires them.
2. Read and grep the bundled docs at `node_modules/ai/docs/` and the source at `node_modules/ai/src/`.
3. Provider and framework packages bundle their own docs at `node_modules/@ai-sdk/<name>/docs/`.
4. If something isn't in the bundled docs, search https://ai-sdk.dev/docs. You can append `.md` to any docs page URL to get its markdown, and search via `https://ai-sdk.dev/api/search-docs?q=your_query`.
5. If you cannot find support for an answer in the docs or source, say so explicitly — do not guess.
```

### vercel.com/docs/agent-resources/vercel-mcp.md - MCP install page (355 lines, 15,870 bytes) with one-click deep links — HTTP 200

<https://vercel.com/docs/agent-resources/vercel-mcp.md>

Per-client MCP install matrix: Claude Code (`claude mcp add --transport http vercel https://mcp.vercel.com`), Claude.ai/Desktop custom connector, Cursor one-click deeplink `cursor://anysphere.cursor-deeplink/mcp/install?name=vercel&config=eyJ1cmwiOiJodHRwczovL21jcC52ZXJjZWwuY29tIn0%3D`, VS Code one-click `vscode:mcp/install?%7B%22name%22%3A%22Vercel%22%2C%22url%22%3A%22https%3A%2F%2Fmcp.vercel.com%22%7D` plus manual palette steps, Windsurf (`serverUrl`), Gemini Code Assist / Gemini CLI (`npx mcp-remote`). Every client gets the exact JSON to merge and the exact post-add auth gesture ('Needs login' click, `/mcp`).

````text
# Add Vercel MCP
claude mcp add --transport http vercel https://mcp.vercel.com

# Start coding with Claude
claude

# Authenticate the MCP tools by typing /mcp
/mcp
```

[...]

### Cursor

[Add to Cursor](cursor://anysphere.cursor-deeplink/mcp/install?name=vercel\&config=eyJ1cmwiOiJodHRwczovL21jcC52ZXJjZWwuY29tIn0%3D)

Click the button above to open Cursor and automatically add Vercel MCP. You can
also add the snippet below to your project-specific or global `.cursor/mcp.json`
file manually.

```json
{
  "mcpServers": {
    "vercel": {
      "url": "https://mcp.vercel.com"
    }
  }
}
```
````

### vercel-labs/skills README - the `skills` CLI that `npx skills add` invokes (591 lines, 27,375 bytes) — HTTP 200

<https://raw.githubusercontent.com/vercel-labs/skills/main/README.md>

Distribution primitive the whole ecosystem's install prompts lean on: one command installs a SKILL.md from any git source into 79 supported agents; `skills use ... | claude` turns a skill into a prompt piped straight into an agent without installing; `-y`/`--all`/`-a` flags for non-interactive CI installs; `skills init` scaffolds a SKILL.md. Also lists `npx skills add owner/repo --global` for user scope (used by the Vercel playbook fallback).

````text
## Install a Skill

```bash
npx skills add vercel-labs/agent-skills
```

## Use a Skill Without Installing

Generate a prompt for one skill, or start a supported coding agent interactively:

```bash
npx skills use vercel-labs/agent-skills@web-design-guidelines | claude
npx skills use vercel-labs/agent-skills --skill web-design-guidelines --agent claude-code
```

`skills use` resolves sources the same way as `skills add`, writes the selected skill files to a temporary directory, and prints only the generated prompt to stdout unless `--agent` is provided. With `--agent`, it starts one supported agent interactively with the generated prompt.
````

### vercel.com/docs/llms-full.txt (platform full docs) — HTTP 200

<https://vercel.com/docs/llms-full.txt>

9,477,371 bytes (HEAD-measured via curl size_download). Not dissected further - same concatenated-dump pattern as ai-sdk.dev/llms-full.txt; vercel.com/llms.txt labels it 'Complete documentation and REST API reference in one file.'

```text
- [Full documentation content](https://vercel.com/docs/llms-full.txt): Complete documentation and REST API reference in one file.
```

### sdk.vercel.ai/llms.txt (legacy domain) — HTTP 200

<https://sdk.vercel.ai/llms.txt>

Redirects to https://ai-sdk.dev/llms.txt (curl url_effective), byte-identical 2,217 bytes. Legacy domain kept alive so stale training-data URLs still resolve - relevant to XERJ's canonical-domain honesty item.

```text
# AI SDK

> The AI SDK is a provider-agnostic TypeScript toolkit for building AI-powered applications and agents with React, Next.js, Vue, Svelte, Node.js, and other JavaScript runtimes.
```

### ai-sdk.dev/llms-install.md — HTTP 500

<https://ai-sdk.dev/llms-install.md>

Does not exist as a usable file: server returned HTTP 500 with a 10,304-byte error page. No llms-install.md for this project.

## What they do better than XERJ

- Capability branching at the top of llms.txt: 'If you can fetch URLs, search the docs first' vs 'If you are working inside a local coding project with filesystem access, install the AI SDK skill first'. XERJ's llms.txt assumes a shell from line 1 (`curl -fsSL https://xerj.org/get | sh`) and only mentions the no-shell path in a link description ('Agent quickstart ... the HTTP-only path for an agent with no shell') - exactly the FTX finding that shell-less personas fail.
- Size and reading cost: 46 lines / 2.2 KB vs XERJ's 234 lines / 40 KB. The AI SDK llms.txt is one screen; an agent reads all of it. XERJ's 'Start here' alone is longer than Vercel's whole file, and its first-run list has two items numbered '4.' (the query step and the field-report step) - a defect an agent reading for structure will notice.
- A queryable search endpoint FOR the docs, given as a URL template with four worked examples: 'https://ai-sdk.dev/api/search-docs?q=your+query' ... 'Fetch the returned URLs with `.md` appended to get Markdown content.' XERJ tells agents to 'search it for your topic' but offers no endpoint for that - the agent has to read 40 KB.
- One install command, zero package-manager detail, and the package install itself is deferred to the skill and made conditional: 'If `node_modules/ai/` does not exist, install **only** the `ai` package'. Install-what-you-need-when-you-need-it, versus XERJ front-loading checksum tools, PATH hints, install dirs, env vars and Windows variants into the first screen.
- The get-started.md playbook is an actual agent prompt with an execution contract: 'Perform actions yourself when terminal or file access is available ... Pause only for user authentication, approval, or UI actions you cannot perform. Do not merely return commands for the user to copy.' plus a check-first step (`vercel --version` / `vercel whoami`) and a fixed 'Report only verified state' completion block. XERJ has no equivalent single page an operator can paste to an agent.
- MCP is shipped and documented per client with one-click links (cursor://..., vscode:mcp/install?...), the exact `claude mcp add` line, JSON to merge for Cursor/Windsurf/Gemini, and the post-add auth gesture. XERJ's MCP server exists (`xerj mcp`, `xerj init` writes `.mcp.json`) but llms.txt gives no per-client snippet, no one-click link, and no '(optional, one command)' path becomes the primary route for MCP-only personas.
- Consistency of the 'Optional' heading: on vercel.com/llms.txt '## Optional' contains only machine catalogs (ai-catalog.json, taxonomy.json, graph.json) - things an agent may ignore. XERJ files its 'required, not optional' field report under a heading literally named 'Optional', and the body says 'the template is under Optional'. Any agent that prunes optional sections drops the obligation - a direct candidate cause for the low PR rate the user is asking about.
- Front-matter metadata on the agent page (get-started.md): `supported_surfaces: [cli, plugin, skills, mcp]` and `runtimes: [Claude Code, Cursor, Codex CLI, VS Code with Copilot, Windsurf, Gemini CLI]` - lets a client or agent decide applicability before reading.
- AGENTS.md is the single source of truth and CLAUDE.md is a 9-byte pointer to it - no drift between agent files.
- Contribution obligations are mechanised, not exhorted: 'Every PR modifying production code needs a changeset' with the command to run, enforced by CI; the skill says nothing about feedback at all. Vercel does not ask agents for field reports; it asks for nothing, and its llms.txt carries no moral tone ('owes', 'your side of the deal').

## What XERJ does better

- Honesty caveats inline and load-bearing: the whole '## Caveats (read before you rely on a capability)' block (lexical-vs-neural default, HNSW recall 'measured, not guaranteed', fusion 'rrf or linear ONLY', RSS-runaway defect with a real 20.2 GB data point). Vercel's llms.txt carries no caveats; the only comparable line is in the skill ('If you cannot find support for an answer in the docs or source, say so explicitly — do not guess').
- Operator-safety guidance for autoindexing on someone else's machine ('Estimate a run before you start it', exit-code table, progress-line protocol, 'never invent a number') - Vercel's docs have nothing about the agent acting on a human's laptop resources.
- A feedback loop at all: `xerj feedback --open-pr --agent ... --used-for ... --verdict ...` with CLA-exempt one-file PRs, `--dry-run` fallback for sandboxed agents, and 'A negative report is a good report'. Vercel asks agents for nothing back, so XERJ's problem is placement/tone, not absence.
- Exact HTTP request bodies for every core operation inline (search/semantic/knn/hybrid/memory) so an HTTP-only agent can act without following a link; the AI SDK llms.txt is links only.
- Tool-schema files for agents (mcp-tools.json, anthropic-tools.json 'generated from a real `tools/list` of the server that serves it') - Vercel offers an OpenAPI description for the REST API but no MCP/tool-use schema files.
- Verified-install story (SHA-256 fail-closed, the manual verifiable install without `curl | sh`) - documented, and per the FTX study it flipped a policy-constrained persona to success; Vercel's install is `npm install --global vercel@latest` with no integrity discussion.
- `xerj init` does in one command what Vercel's playbook spreads over three sections (writes `.mcp.json`, writes a skill, backs up edited files) - XERJ just does not lead with it.

## Adoptable ideas

- Add capability-branched top sections modelled on https://ai-sdk.dev/llms.txt lines 7-30: '## Web Access' -> 'If you can fetch URLs, ...' and '## Local Coding Agents' -> 'If you are working inside a local coding project with filesystem access, install ... first'. For XERJ: '## If you have a shell' (`curl -fsSL https://xerj.org/get | sh`), '## If you can only fetch URLs' (the HTTP-only quickstart, promoted from a link description to a section), '## If you have MCP but no shell' (`xerj mcp` / `.mcp.json` snippet). This is the FTX shell-less-persona fix in the shape a 27k-star project already uses.
- Ship the install-and-verify prompt as one pasteable page with an execution contract, copying the wording pattern from https://vercel.com/get-started.md: 'Perform actions yourself when terminal or file access is available. Global or user-scoped installation is the default. Pause only for user authentication, approval, or UI actions you cannot perform. Do not merely return commands for the user to copy.' Precede install with a check-first step (theirs: `vercel --version` / `vercel whoami`; XERJ: `xerj --version` then `curl -s http://localhost:9200/_cluster/health`).
- End that page with a mandatory fixed-format completion block - the field report can BE this block. Verbatim pattern from get-started.md: '## Completion\n\nReport only verified state:\n\n```text\n▲ Vercel agent setup is ready\nCLI: <version>, authenticated as <username>\nGuidance: <plugin|skills|skipped>, global or user scope\nMCP: <connected|skipped>, ...\nAuthenticated MCP check: <list_teams succeeded|not requested>\n```'. XERJ version: 'XERJ setup is ready / Version: <xerj --version> / Node: <health> / Indexed: <folder, N docs, exit code> / Embed mode: lexical|neural / Field report: <PR URL | dry-run handed to operator>'. An agent that must print 'Field report: ...' to finish cannot silently skip it - this addresses the low-PR-rate concern by making the report a completion artifact instead of a moral obligation.
- Rename and relocate: never put a required item under '## Optional'. On https://vercel.com/llms.txt the '## Optional' section holds only 'Agent resource catalog ... Experimental catalog', 'Product taxonomy', 'Documentation graph' - things safe to skip. Move the field-report template link into the first-run list (already step 4) and into a section named e.g. '## Before you end the session' ; keep '## Optional' for schema/catalog files only (mcp-tools.json, anthropic-tools.json), matching Vercel's semantics. Also fix the duplicated '4.' numbering in Start here.
- Add a docs search endpoint and advertise it with worked examples exactly as ai-sdk.dev does: '- Search endpoint: https://ai-sdk.dev/api/search-docs?q=your+query' ... 'The search endpoint returns JSON with documentation URLs. Fetch the returned URLs with `.md` appended to get Markdown content.' XERJ already serves answers/*.md and could expose e.g. https://xerj.org/api/search-docs?q=... backed by XERJ itself (dogfood), and note that every docs page is available as `.md`. This also lets llms.txt shrink toward Vercel's 46 lines.
- Publish a SKILL.md and make `npx skills add xerj-org/xerj` the one-line 'Local Coding Agents' install, as ai-sdk.dev does with 'npx skills add vercel/ai' ... 'Then follow the skill instructions before changing code.' The `skills` CLI (https://raw.githubusercontent.com/vercel-labs/skills/main/README.md) installs into 79 agents and supports `npx skills use xerj-org/xerj | claude` for install-free prompting. Put the honesty caveats and the field-report step inside the SKILL.md so they travel with the install. Model the skill's opening on skills/use-ai-sdk/SKILL.md: 'Never write ... code from memory. Always verify every API, option, and pattern against the documentation ... for the version that is actually installed' -> for XERJ: 'Never describe search as neural from memory - check `--embed-mode` on the running node first' (fixes the 9-of-12 misread).
- Per-client MCP install matrix with one-click links, copied from https://vercel.com/docs/agent-resources/vercel-mcp.md: the `claude mcp add --transport http vercel https://mcp.vercel.com` / `claude` / `/mcp` three-liner, the Cursor deeplink form `cursor://anysphere.cursor-deeplink/mcp/install?name=<name>&config=<base64 json>`, the VS Code form `vscode:mcp/install?<urlencoded {name,url}>`, and the `"mcpServers": { "<name>": { "url": ... } }` JSON to merge. XERJ's stdio server would be `{"mcpServers":{"xerj":{"command":"xerj","args":["mcp"]}}}` plus `claude mcp add xerj -- xerj mcp`; publish it at a stable URL and link it from the MCP-only branch of llms.txt.
- Front-matter on agent pages, from get-started.md: `supported_surfaces: [cli, plugin, skills, mcp]` and `runtimes: [Claude Code, Cursor, Codex CLI, VS Code with Copilot, Windsurf, Gemini CLI]` plus a `description: |- Hand your coding agent this playbook to ...` line written for the human who will paste it. And collapse CLAUDE.md to a pointer whose entire content is 'AGENTS.md' (https://raw.githubusercontent.com/vercel/ai/main/CLAUDE.md, 9 bytes) so the two never drift.
