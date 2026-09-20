# Supabase

Category: database platform

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://supabase.com/llms.txt> — HTTP 200
- Size: 2709 bytes, 43 lines
- Install position: absent — no install command anywhere in llms.txt. The only agent-relevant line is the last one: '- [Supabase MCP server](https://mcp.supabase.com/mcp): Streamable HTTP MCP endpoint, OAuth-protected, for managing projects, database schema, and queries from MCP clients'. Installation lives on the linked AI Tools guide (https://supabase.com/docs/guides/ai-tools.md) and the MCP page.
- Tone: neutral — a plain auto-generated docs index. It makes NO demands of the agent: no imperatives, no 'you must', no contribution ask. The only instruction-like sentence is 'For the complete documentation in a single file, see [Full Documentation](https://supabase.com/llms-full.txt).'
- Section order: # Supabase Docs (one-line pointer to llms-full.txt) → ## Documentation (20 guide-area .md links + 9 per-SDK reference dumps /llms/js.txt, /llms/dart.txt, /llms/swift.txt, /llms/kotlin.txt, /llms/python.txt, /llms/csharp.txt, /llms/server.txt, /llms/cli.txt, /llms/api.txt) → ## Pricing → ## API and agent resources (openapi.json + MCP endpoint)

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
## API and agent resources

- [Supabase Management API OpenAPI spec](https://supabase.com/openapi.json): OpenAPI 3.0 description of the Management API for managing organizations, projects, branches, and configuration
- [Supabase MCP server](https://mcp.supabase.com/mcp): Streamable HTTP MCP endpoint, OAuth-protected, for managing projects, database schema, and queries from MCP clients
```

## llms-full.txt

- URL: <https://supabase.com/llms-full.txt> — HTTP 200
- Size: 7022257 bytes
- Relationship to llms.txt: Concatenation, not curation: llms.txt is a link index to per-page .md renders (every docs URL + '.md' returns markdown, e.g. /docs/guides/ai-tools/mcp.md); llms-full.txt is those same .md pages concatenated in the llms.txt order, prefixed by pricing.md. Interactive components are stripped and leave blank gaps (the '## Prompts' section body is empty; the MCP page's 'Choose your Supabase platform, project, and MCP client' configurator and the Cursor one-click button are absent from the markdown, so no cursor:// deeplink survives). The 9 per-SDK reference dumps are separate files, not inlined.
- Install position: Order: '# Supabase' → '## Pricing' (lines 3-260, the pricing.md page verbatim) → '## Documentation' → '# Supabase Guides' → '# AI Tools' (line 264) → '# AI Prompts' (309) → '# Agent Skills' (341, first install commands at line 348: `npx skills add supabase/agent-skills`) → '# Deploy MCP servers' (411) → '# Supabase MCP Server' (~line 800; `claude mcp add --scope project --transport http supabase "https://mcp.supabase.com/mcp"` at line 819) → '# Supabase Plugin for AI Coding Agents' (line 1413: `npx plugins add supabase-community/supabase-plugin`) → '# AI & Vectors' → the rest of the ~1,356 top-level guide pages. So the agent-tooling install story is the FIRST documentation content after pricing, within the first ~1,500 of 178,984 lines.

## Other agent-facing files

### MCP install page (markdown render) — HTTP 200

<https://supabase.com/docs/guides/ai-tools/mcp.md>

The whole install story. 21 KB / 615 lines. Headings: Remote MCP installation → per-client blocks under '#### AI Agent CLI' (Claude Code, Codex, Grok, Gemini CLI, Factory, fx, OpenCode/omp, Kiro, Kimi, Warp…), '#### Web Clients' (Claude.ai, ChatGPT connectors, Goose), '#### IDE' (Cursor incl. Cursor CLI, VS Code…) → 'Next steps' (auth + verify) → 'Available tools' by feature group → 'Configuration options' (`read_only=true`, `project_ref=<id>`, `features=<groups>`) → 'Manual authentication' incl. 'CI environment' → 'Security risks' (Prompt injection, Recommendations) → 'On GitHub'. Every client block has the SAME shape: one CLI command, then 'Alternatively, add this configuration to <file>:' with a JSON/TOML/YAML snippet, then an authenticate step, then a verify step. Opens with 'Caution: Connecting an LLM to your Supabase projects carries security risks. Read our [security best practices](#security-risks) before running the MCP server.' Verify step is a natural-language prompt: 'To verify the client has access to the MCP server tools, try asking it to query your project or database using natural language. For example: "What tables are there in the database? Use MCP tools."' The 'Configure your MCP client' also states: 'Your MCP client will automatically prompt you to log in to Supabase during setup.' No feedback/contribution ask on the page.

````text
**Claude Code**

Add the MCP server to your project config using the command line:

```bash
claude mcp add --scope project --transport http supabase "https://mcp.supabase.com/mcp"
```

Alternatively, add this configuration to `.mcp.json`:

```json
{
  "mcpServers": {
    "supabase": {
      "type": "http",
      "url": "https://mcp.supabase.com/mcp"
    }
  }
}
```

After configuring the MCP server, you need to authenticate. In a regular terminal (not the IDE extension) run:

```bash
claude /mcp
```

Select the "supabase" server, then "Authenticate" to begin the authentication flow.
````

### AI Tools hub page (inside llms-full, line 264; also https://supabase.com/docs/guides/ai-tools.md) — HTTP 200

<https://supabase.com/docs/guides/ai-tools.md>

The four-lane taxonomy that routes every agent persona to a working path: MCP (live connection), Agent Skills (portable instructions, no connection needed), Plugin (bundle of both), Prompts (static files for tools that support none of the above). 'Pick your agent' lists ~20 agents each linking to either the mcp or plugins page. Links to an open-source eval: 'See how these tools perform on real Supabase tasks in [Supabase Evals](https://supabase.com/evals), our open-source benchmark for AI coding agents.'

```text
Supabase provides everything you need to connect an AI coding agent to your project: a live connection to your database and platform (MCP), portable instructions your agent can reuse (Agent Skills), a one-step bundle of both (Plugin), and copy-paste prompts for tools that don't support any of the above.

## Key concepts

- **MCP (Model Context Protocol)**: a live connection between your agent and your actual Supabase project. Once connected, your agent can call tools to query data, run migrations, deploy Edge Functions, and more.
- **Agent Skills**: portable, on-demand instructions your agent loads when it needs Supabase- or Postgres-specific procedural knowledge. Skills don't require a live connection, and work across different agents.
- **Plugin**: a single install that bundles the MCP server and Agent Skills together for a specific agent.
- **Prompts**: static prompt files you copy into your project for agents that don't support MCP, plugins, or skills natively.
```

### Agent Skills page (inside llms-full line 341; https://supabase.com/docs/guides/ai-tools/ai-skills.md) — HTTP 200

<https://supabase.com/docs/guides/ai-tools/ai-skills.md>

Install via the vercel-labs `skills` CLI; each skill's description is written as a TRIGGER SPEC telling the agent when to load it ('Use when doing ANY task involving Supabase. Triggers: …'; 'Load this skill BEFORE writing or changing anything that lives in a Postgres database…'). Install: `npx skills add supabase/agent-skills`, `npx skills add supabase/agent-skills --skill SKILL_NAME`, update: `npx skills update`, discover: `npx skills find QUERY`. Scope note: 'Skills are installed at project scope by default, placing them in your repository so contributors and cloud agents all share the same setup. Pass `--global` to install across all your projects instead.'

````text
### supabase-postgres-best-practices
      
Postgres best practices maintained by Supabase, for Postgres running anywhere. Load this skill BEFORE writing or changing anything that lives in a Postgres database: creating or altering tables and columns (including choosing column types), schema design, migrations and declarative schema files, RLS policies and the tests that verify them, indexes, triggers, database functions, queues and scheduled jobs (pg_cron, pgmq), vector/semantic search (pgvector), and restoring dumps (pg_restore) or importing data. Also load it when diagnosing slow queries, high CPU, timeouts, EXPLAIN plans, connection exhaustion, locking, bloat, or rows visible to the wrong user or tenant. This is not just a performance guide — schema, migration, security, and SQL authoring tasks need these rules too, even for a one-column change or a single query.
      
```sh
npx skills add supabase/agent-skills --skill supabase-postgres-best-practices
```
````

### Plugin page (inside llms-full line 1413; https://supabase.com/docs/guides/ai-tools/plugins.md) — HTTP 200

<https://supabase.com/docs/guides/ai-tools/plugins.md>

One command that auto-detects every installed coding agent and installs MCP + skills into all of them.

````text
## Quick installation

```bash
npx plugins add supabase-community/supabase-plugin
```

The [`plugins`](https://www.npmjs.com/package/plugins) package auto-detects your installed AI coding agents and installs the Supabase plugin to all of them with one command. Use `--yes` to skip the confirmation prompt.
````

### AGENTS.md (monorepo) — HTTP 200

<https://raw.githubusercontent.com/supabase/supabase/master/AGENTS.md>

Contributor-agent guide for the monorepo, 6,604 bytes / 75 lines: structure table, common commands, CI gates, conventions, and a skills index under `.agents/skills/` that agents must load before working. Not product install. Notable: a 'Public surfaces' rule about what agents must keep out of world-readable PR text.

```text
## Skills

The skills in `.agents/skills/` are the source of truth for conventions — load the relevant ones before working, don't guess:
```

### CLAUDE.md (monorepo) — HTTP 200

<https://raw.githubusercontent.com/supabase/supabase/master/CLAUDE.md>

11-byte pointer file so Claude Code reads AGENTS.md.

```text
@AGENTS.md
```

### supabase-mcp README — HTTP 200

<https://raw.githubusercontent.com/supabase-community/supabase-mcp/main/README.md>

8,088 bytes / 165 lines. Setup is two steps: '### 1. Follow our security best practices' then '### 2. Configure your MCP client', with one universal JSON snippet and a fallback rule for unlisted clients. Also documents AI-SDK typed tool schemas (`createToolSchemas()`) and self-hosting the MCP handler. Contribution ask is a single pointer: 'See [CONTRIBUTING](./CONTRIBUTING.md) for details on how to contribute to this project.' No field-report/feedback ask.

````text
Most MCP clients require the following information:

```json
{
  "mcpServers": {
    "supabase": {
      "type": "http",
      "url": "https://mcp.supabase.com/mcp"
    }
  }
}
```

If you don't see your MCP client listed in our documentation, check your client's MCP documentation and copy the above MCP information into their expected format (json, yaml, etc).
````

### llms.txt at docs root — HTTP 404

<https://supabase.com/docs/llms.txt>

does not exist (returns the docs 404 page)

### llms-install.md — HTTP 404

<https://supabase.com/llms-install.md>

does not exist

### SKILL.md at site root — HTTP 404

<https://supabase.com/SKILL.md>

does not exist (skills live in github.com/supabase/agent-skills, installed via `npx skills add`)

### llms.txt at repo root — HTTP 404

<https://raw.githubusercontent.com/supabase/supabase/master/llms.txt>

does not exist

## What they do better than XERJ

- Persona routing by capability, not by product: the AI Tools hub names four lanes — MCP (needs live connection), Skills (no connection needed, portable), Plugin (bundle), Prompts ('for tools that don't support any of the above') — so a shell-less or MCP-only agent always has a documented path. XERJ's llms.txt assumes a shell for steps 1-4 and points shell-less agents at one 'Agent quickstart' link.
- Identical per-client install shape, repeated for ~20 clients: one CLI command → 'Alternatively, add this configuration to <file>:' + verbatim config snippet → authenticate step → verify step. An agent can pattern-match its own client in seconds. XERJ documents `xerj init` (Claude Code/Cursor only) and gives no `.mcp.json` snippet inline.
- A natural-language verify prompt: 'try asking it to query your project or database using natural language. For example: "What tables are there in the database? Use MCP tools."' — first-use is defined as a prompt the agent can execute, not a command.
- Security caution BEFORE install, with concrete scoping knobs an agent can apply to itself (`?read_only=true`, `?project_ref=<id>`, `?features=database,docs`), plus an explicit unattended-agent rule: 'The routine must stop and report a recommendation instead of running a write operation.'
- Skill descriptions written as trigger specs ('Use when doing ANY task involving Supabase. Triggers: …', 'Load this skill BEFORE writing or changing anything…') so the agent knows WHEN to load, not just what exists.
- One-command all-agents install: `npx plugins add supabase-community/supabase-plugin` auto-detects installed agents.
- Every docs URL has a `.md` twin (append `.md`), and llms-full.txt is the deterministic concatenation of those — nothing in llms-full is hand-written or drifts from the docs.
- Install and first-use docs contain zero obligations and zero marketing; the tone is entirely procedural.

## What XERJ does better

- Supabase's llms.txt is a plain auto-generated docs index with nothing about installation — an agent hitting only llms.txt learns no install command, no MCP snippet, no first-use prompt. XERJ's llms.txt has the install commands in the first 15 lines.
- XERJ has an inline Caveats section correcting known agent misreadings (lexical-vs-neural, PATH, data-dir placement). Supabase's llms.txt/llms-full has no 'where agents get this wrong' content at all.
- XERJ has a contribution/field-report mechanism (`xerj feedback --open-pr`, CLA-exempt one-file PR) and an AI contribution protocol; Supabase asks agents for nothing back (README: only 'See CONTRIBUTING').
- XERJ's llms.txt is a curated, sized index (40 KB) an agent can read in one hop; Supabase's llms-full is 7 MB / 178,984 lines and unreadable in one context, and its llms.txt is too thin to act on.
- XERJ documents the manual verified install path, checksum behaviour and the PATH consequence for a policy-constrained persona; Supabase has no equivalent (hosted service, nothing to install locally besides MCP config).
- XERJ's llms.txt states measured numbers with provenance; Supabase's llms.txt has none (its evals live on a separate page).

## Adoptable ideas

- Add a four-lane 'Pick your path' block to XERJ llms.txt modelled on https://supabase.com/docs/guides/ai-tools.md Key concepts: '**MCP**: a live connection between your agent and your actual … project' / '**Agent Skills**: … don't require a live connection, and work across different agents' / '**Plugin**: a single install that bundles the MCP server and Agent Skills together' / '**Prompts**: static prompt files you copy into your project for agents that don't support MCP, plugins, or skills natively.' Map to XERJ: shell lane (`curl | sh` + `xerj autoindex`), MCP-only lane (needs the MCP server actually shipped + an `.mcp.json` snippet), HTTP-only lane (POST /_search), and a no-tools lane (paste-in prompt). This directly addresses the shell-less personas that fail at every tier.
- Inline the MCP snippet in XERJ llms.txt using Supabase's exact block shape from https://supabase.com/docs/guides/ai-tools/mcp.md: '**Claude Code** … `claude mcp add --scope project --transport http supabase "https://mcp.supabase.com/mcp"` … Alternatively, add this configuration to `.mcp.json`: {"mcpServers":{"supabase":{"type":"http","url":…}}}' — one command, one config file, one verify step — and add Supabase's fallback sentence for unlisted clients: 'If you don't see your MCP client listed in our documentation, check your client's MCP documentation and copy the above MCP information into their expected format (json, yaml, etc).'
- Define first-use as a natural-language prompt, copying the pattern 'To verify the client has access to the MCP server tools, try asking it to query your project or database using natural language. For example: "What tables are there in the database? Use MCP tools."' XERJ equivalent: 'Ask: "Which indices exist and what fields do they have? Use xerj tools."' — this gives MCP-only personas a success criterion that does not require a shell.
- Write XERJ's skill/tool descriptions as trigger specs, verbatim pattern from the Agent Skills page: 'Use when doing ANY task involving Supabase. Triggers: …' and 'Load this skill BEFORE writing or changing anything that lives in a Postgres database…'. E.g. the `xerj init` 10-line skill and the `xerj_search` MCP tool description should say WHEN to call them ('Use BEFORE writing any non-trivial code: retrieve how the nearest open-source project solved it…').
- Move the field-report obligation out of the '## Optional' heading and give it the same procedural shape Supabase gives auth: a numbered step with command → verify → fallback. Supabase has no contribution ask to copy, but its structural lesson applies: the doc never contradicts itself (nothing 'required' is filed under 'optional'). Rename the section '## Your side of the deal (required)' or fold the report into Start-here step 5 and delete the 'Optional' heading; also fix the duplicated '4.' numbering in Start here.
- Adopt the pre-install security caution and self-scoping knobs from the MCP page: 'Caution: Connecting an LLM to your … carries security risks. Read our [security best practices] before running the MCP server.' plus scoping parameters (`?read_only=true`, `?project_ref=<id>`, `?features=…`) and the unattended-agent rule 'The routine must stop and report a recommendation instead of running a write operation.' XERJ analogue: a read-only MCP mode / index-prefix scoping documented next to the snippet.
- Offer a one-command all-agents install like 'npx plugins add supabase-community/supabase-plugin — auto-detects your installed AI coding agents and installs the Supabase plugin to all of them with one command. Use `--yes` to skip the confirmation prompt.' XERJ already has `xerj init` (Claude Code/Cursor); document which agents it detects and add `--yes`, and publish the skill to the vercel-labs `skills` registry so `npx skills add xerj-org/agent-skills` works for agents that cannot run the binary.
- Publish `.md` twins for every docs page (Supabase: append `.md` to any /docs URL, e.g. https://supabase.com/docs/guides/ai-tools/mcp.md) and generate llms-full.txt by concatenating them, so llms-full never drifts from the site; keep XERJ's llms.txt curated (Supabase's 43-line index is a cautionary example of what NOT to do — it contains no install path at all).
