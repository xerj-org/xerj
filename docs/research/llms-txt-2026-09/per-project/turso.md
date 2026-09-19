# Turso

Category: database (SQLite-compatible)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.turso.tech/llms.txt> — HTTP 200
- Size: 33136 bytes, 292 lines
- Install position: No install instructions in the file itself. Installation appears only as index links: line 27 '- [Getting Started](https://docs.turso.tech/sql-reference/cli/getting-started.md): Install and use the Turso interactive SQL shell', line 161 '- [Installation](https://docs.turso.tech/cli/installation.md): Learn how to install the Turso CLI on Mac, Linux and Windows.', line 271 '- [Installation](https://docs.turso.tech/agentfs/installation.md): Install the AgentFS CLI on Linux, macOS, and Windows'. Line 6 [Quickstart](https://docs.turso.tech/tursodb/quickstart.md) is the second link in the file but carries no description. This is a plain Mintlify auto-generated docs index with nothing about installation beyond link titles.
- Tone: neutral / catalog. Zero imperative sentences aimed at the agent; makes no demands. Every line is '- [Title](url.md): one-line description'. The only marketing phrase is the line-5 tagline 'The small database to power your big dreams in the age of AI.' Note: every linked .md page opens with a 3-line banner '> ## Documentation Index / > Fetch the complete documentation index at: https://docs.turso.tech/llms.txt / > Use this file to discover all available pages before exploring further.' — the only agent-directed instruction in the whole system, and it points back to the index, not to install.
- Section order: # Turso → ## Docs (lines 3-282, ~280 flat bullet links, no sub-headings; order of appearance: Welcome/Quickstart, Connect (JS/Py/Go/Rust/Dart/Java), CDC/Concurrent Writes/Encryption, Vector Search, Sync, Agent Databases / AI Memory / Code Indexing (lines 22-24), SQL shell, SDKs, CLI incl. line 161 [Installation] and line 175 [contact feedback], Cloud, integrations incl. line 268 [MCP (AI agents)], AgentFS block lines 270-282) → ## OpenAPI Specs (line 284) → ## Optional (line 288: Homepage, Discord, GitHub)

### Install commands found

```sh
(none in docs llms.txt itself) — linked pages contain: curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tursodatabase/turso/releases/latest/download/turso_cli-installer.sh | sh
irm https://github.com/tursodatabase/turso/releases/latest/download/turso_cli-installer.ps1 | iex
brew install tursodatabase/tap/turso
curl -sSfL https://get.tur.so/install.sh | bash
curl -fsSL https://github.com/tursodatabase/agentfs/releases/latest/download/agentfs-installer.sh | sh
npm install @tursodatabase/database
pip install pyturso
go get turso.tech/database/tursogo
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

````text
(none in llms.txt; from https://docs.turso.tech/integrations/mcp.md, Cursor tab)
```json theme={null}
{
  "mcpServers": {
    "turso": { "url": "https://mcp.turso.ai/mcp" }
  }
}
```

(Codex tab, same page) MCP-only alternative: add `[mcp_servers.turso]` with
`url = "https://mcp.turso.ai/mcp"` to `~/.codex/config.toml`, then
`codex mcp login turso`.

(Other MCP clients tab) Point any MCP client that supports remote (Streamable HTTP) servers with OAuth
at:
```
https://mcp.turso.ai/mcp
```
The client discovers the authorization server automatically (RFC 9728 / RFC
8414\) and walks you through the same browser approval.
````

### Feedback / contribution ask (verbatim)

````text
(only an index link, line 175) - [contact feedback](https://docs.turso.tech/cli/contact/feedback.md)

(that page, in full:) # contact feedback

You can send feedback to the Turso team by using the following command:

```bash theme={null}
turso contact feedback
```

(## Optional section, lines 288-292:)
- [Turso Homepage](https://turso.tech)
- [Discord](https://tur.so/discord)
- [GitHub](https://github.com/tursodatabase/turso)
````

### Best passage for driving install and first use (verbatim)

```text
(There is no install-driving passage in docs.turso.tech/llms.txt. The closest agent-usable passage in the Turso llms surface is the SECOND, hand-written file at https://turso.tech/llms.txt (200, 377 lines, 11,435 bytes), which the task list did not know about:)

## Which package should I use?

| Use case | TypeScript | Python |
| --- | --- | --- |
| **Local database** (embedded, on-device, offline) | `@tursodatabase/database` | `pyturso` |
| **Local database + cloud sync** (push/pull) | `@tursodatabase/sync` | `pyturso` (with sync) |
| **Remote access** (servers, Docker, serverless, edge — any over-the-wire) | `@tursodatabase/serverless` | `libsql` |
| **Legacy (libSQL)** — battle-tested, ORM support | `@libsql/client` | `libsql` |

**Starting a new project?** Use `@tursodatabase/database` (TypeScript) or `pyturso` (Python) for local/embedded use. Use `@tursodatabase/serverless` for any application that connects to a remote Turso Cloud database over the network — including Node.js servers, Docker containers, serverless functions, and edge runtimes. Use `@libsql/client` only if you need ORM integration (Drizzle, Prisma) today.

**Need sync?** Use `@tursodatabase/sync` (TypeScript) or `pyturso` with sync (Python) for local reads and writes with explicit push/pull to Turso Cloud.
```

### One-click links

- `(none in docs llms.txt) — linked MCP page https://docs.turso.tech/integrations/mcp.md has: cursor://anysphere.cursor-deeplink/mcp/install?name=turso&config=eyJ1cmwiOiJodHRwczovL21jcC50dXJzby5haS9tY3AifQ==`
- `/plugin marketplace add tursodatabase/turso-mcp`
- `/plugin install turso@turso`
- `codex plugin marketplace add tursodatabase/turso-mcp`
- `codex plugin add turso@turso`
- `codex mcp login turso`

## llms-full.txt

- URL: <https://docs.turso.tech/llms-full.txt> — HTTP 200
- Size: 1083272 bytes
- Relationship to llms.txt: Mintlify auto-concatenation of every docs page in llms.txt, each page as '# Title / Source: <url> / description / body' with MDX tags (<Tabs>, <Steps>, <Accordion>) left in and 'theme={null}' on every code fence. Not curated; same 417 '# ' page headings. Contains 0 occurrences of the per-page 'Documentation Index' banner (that banner is added only when a single .md page is fetched).
- Install position: First install text at line 966 of 33,351 ('# Installation / Source: https://docs.turso.tech/agentfs/installation' — AgentFS, not the core product; '## Quick Install' at line 971, curl installer at line 976). The Turso CLI install is at line 5249 ('# Installation / Source: https://docs.turso.tech/cli/installation', brew at 5259, get.tur.so at 5265). The tursodb Quickstart installer (turso_cli-installer.sh) is at line 33270 — the last ~0.2% of the file. Page order is alphabetical-by-path-ish, starting with AgentFS guides, so a truncating reader sees AgentFS before Turso.

## Other agent-facing files

### turso.tech/llms.txt (second, hand-written site-level file — not in the task's list) — HTTP 200

<https://turso.tech/llms.txt>

377 lines / 11,435 bytes. Prose + code, not an index: Overview → Which package should I use? (decision table) → Turso Sync → Turso Cloud → SDKs (TS/RN/Python/Rust/Go, each with install + 10-line example) → AgentFS → CLI (command cheat-sheet) → Platform API (endpoints + MCP pointer) → Pricing → Links. This is the file an agent should actually read; docs.turso.tech/llms.txt is the crawler index.

```text
MCP server (hosted, Streamable HTTP): https://mcp.turso.ai/mcp — manifest at
https://turso.tech/.well-known/mcp
```

### AGENTS.md — HTTP 200

<https://raw.githubusercontent.com/tursodatabase/turso/main/AGENTS.md>

165 lines / 8,971 bytes. Contributor-agent guide for the Turso repo itself (build/test commands, test organisation, crate map 'Where to Look' table, commit-message format, core principles, style rules). Nothing about installing or using Turso as a product; it is for agents editing Turso's source.

```text
5. **Own your regressions.** If tests fail after your change, they are your regressions. Debug them directly. Never stash/revert to "check if they fail on main" — that wastes time and is categorically banned.
6. **Validate your hypotheses.**: If you suspect a given cause for a bug, validate it and provide incontrovertible evidence. NEVER make unearned assumptions.
```

### CLAUDE.md — HTTP 200

<https://raw.githubusercontent.com/tursodatabase/turso/main/CLAUDE.md>

9 bytes, a pointer file. Entire content is the string 'AGENTS.md' (no trailing newline: cat -A shows 'AGENTS.md' with no $).

```text
AGENTS.md
```

### MCP (AI agents) docs page — HTTP 200

<https://docs.turso.tech/integrations/mcp.md>

142 lines / 5,509 bytes. The install page for the hosted MCP server: per-client <Tabs> (Claude Code plugin, Codex plugin, Cursor plugin + cursor:// deeplink + mcp.json, Claude web/desktop custom connector, Other MCP clients), OAuth consent scoping, 'How it works' (server holds no privilege). No shell needed for any tab.

```text
Authentication is **OAuth 2.1** (the same model you use to log in to the
dashboard): you approve access in the browser and the agent receives a scoped
token. **There's no API token to copy or paste.**
```

### turso-mcp repo README (plugin marketplace) — HTTP 200

<https://raw.githubusercontent.com/tursodatabase/turso-mcp/main/README.md>

One repo doubles as a plugin marketplace for three agent CLIs (.claude-plugin/, .codex-plugin/, .cursor-plugin/ + .agents/plugins/marketplace.json), each dir bundling .mcp.json + skills/turso/SKILL.md. MIT.

```text
The MCP server is the same for every agent; this repo holds the per-agent
packaging. It also doubles as the plugin **marketplace** each agent's CLI can add.
```

### SKILL.md (bundled in the Claude Code plugin) — HTTP 200

<https://raw.githubusercontent.com/tursodatabase/turso-mcp/main/claude-code/skills/turso/SKILL.md>

Agent-facing skill with frontmatter name/description; disambiguates the Turso/libSQL/Turso Cloud names, lists packages, and gives a numbered 'Which one to recommend (decide in this order)' decision procedure keyed to MCP tool calls (create_database with use_tursodb: true).

```text
## Get the ecosystem right (read this first)

These names get confused constantly. Keep them straight:

- **Turso** — a SQLite-compatible database **engine** (a rewrite of SQLite, the
  successor to libSQL). It runs **local/embedded** and is also an engine option
  on Turso Cloud. **Recommend Turso anywhere you would otherwise reach for
  SQLite.**
```

### .well-known/mcp manifest — HTTP 200

<https://turso.tech/.well-known/mcp>

JSON discovery manifest for the hosted MCP server (name, endpoint, transport streamable-http, oauth2 authorization metadata URLs, documentation, openapi).

```text
"endpoint": "https://mcp.turso.ai/mcp",
  "transport": "streamable-http",
```

### llms-install.md — HTTP 404

<https://docs.turso.tech/llms-install.md>

does not exist

### repo-root llms.txt / SKILL.md / .cursorrules — HTTP 404

<https://raw.githubusercontent.com/tursodatabase/turso/main/llms.txt>

none of these exist at the repo root (llms.txt 404, SKILL.md 404, .cursorrules 404). docs.turso.tech/mcp.md and /ai/mcp also 404 as reported.

## What they do better than XERJ

- Two-file split: docs.turso.tech/llms.txt is a pure crawler index (292 links), while turso.tech/llms.txt (377 lines, 11 KB) is a hand-written agent briefing with a 'Which package should I use?' decision table, one install line + ~10-line code sample per SDK, a CLI command cheat-sheet, and a one-line MCP pointer. An agent gets a working call in under 100 lines; XERJ's 40 KB single file front-loads caveats, PATH warnings and obligations before the first query.
- MCP is shell-free end to end: hosted Streamable HTTP endpoint (https://mcp.turso.ai/mcp) + OAuth consent in the browser, with a .well-known/mcp discovery manifest, a cursor:// deeplink, a Claude web/desktop 'custom connector' path, and a plugin-marketplace repo that packages .mcp.json + SKILL.md per agent CLI (Claude Code, Codex, Cursor). This is exactly the persona (no terminal, MCP-only) that XERJ's 22-run study says fails at every tier.
- Every linked .md page carries a 3-line agent banner pointing back to the index ('Fetch the complete documentation index at: https://docs.turso.tech/llms.txt / Use this file to discover all available pages before exploring further.'), so an agent that lands on any deep page can re-orient.
- The bundled SKILL.md is a decision procedure ('Which one to recommend (decide in this order)' 1-4), not a feature list — it tells the agent what to do when a user asks, and which MCP tool argument to pass (create_database with use_tursodb: true).
- Zero demands, zero obligations, zero 'required' language. The whole install surface is friction-reduction ('no API token to copy, no environment variable, no secret in the repo'). Nothing asks the agent for a report, a PR or a star.
- Use-case pages are first-class one-word index entries (Agent Databases / AI Memory / Code Indexing at lines 22-24) rather than long inline paragraphs.

## What XERJ does better

- XERJ's llms.txt actually contains install commands and a numbered first-run sequence at the top ('## Start here' lines 9-38: curl | sh, irm | iex, then xerj --insecure, xerj autoindex, xerj init, xerj search); docs.turso.tech/llms.txt has zero install text and turso.tech/llms.txt defers the CLI install to a URL ('Installation: https://docs.turso.tech/cli/installation').
- XERJ's llms-full.txt is curated (84 KB, 1,235 lines, numbered sections like §10) and readable in one context window; Turso's is a 1.08 MB, 33,351-line MDX concatenation with <Tabs>/<Steps>/'theme={null}' noise, ordered so AgentFS precedes the core product and the core Quickstart installer sits at line 33,270.
- XERJ documents honest caveats for the agent (lexical default, PATH consequence, data-dir-outside-index trap, exit codes, progress lines, estimate-before-running for the human operator). Turso's files contain no operational caveats at all.
- XERJ has a manual verified-install path and a sandboxed/no-gh fallback for the report ('Run --dry-run, then hand the printed report to your operator'); Turso has no equivalent for restricted environments.
- XERJ has a feedback mechanism with a concrete command and destination (xerj feedback --open-pr → user-feedback/16-agent-field-reports/, CLA-exempt); Turso's only feedback surface is 'turso contact feedback' with no stated destination and no agent ask.
- XERJ's llms.txt has a real section structure (Start here / Caveats / Core agent operations / Answers / Comparisons / Use cases and recipes / Docs / Optional) versus Turso's single flat ## Docs list.

## Adoptable ideas

- Split the file the way Turso does: keep https://xerj.org/llms.txt as a short hand-written briefing (Turso's turso.tech/llms.txt is 377 lines / 11 KB with headings 'Overview', 'Which package should I use?', 'SDKs', 'CLI', 'Platform API', 'Links') and move the 280-line link catalog to llms-full.txt or a /docs index. Turso's decision-table opener is the template: '| Use case | TypeScript | Python |' → for XERJ '| I have… | shell | MCP-only | HTTP-only |' with one command/URL per cell.
- Ship a shell-free MCP path and say so in one sentence, copying Turso's phrasing from https://docs.turso.tech/integrations/mcp.md: 'Connect any MCP-capable AI agent … **There's no API token to copy or paste.**' — i.e. a per-client <Tabs> block (Claude Code / Codex / Cursor / Claude web & desktop / Other MCP clients) where every tab works without a terminal. This directly targets the persona that failed at every tier in the 22-run study.
- Publish a Cursor one-click deeplink exactly in Turso's form: cursor://anysphere.cursor-deeplink/mcp/install?name=turso&config=<base64 of {"url":"https://mcp.turso.ai/mcp"}> — for XERJ, base64 of the stdio/http config that `xerj init` writes into .mcp.json — plus the 'Add to Cursor' anchor text and the manual '~/.cursor/mcp.json' fallback beneath it.
- Add a plugin-marketplace repo modelled on https://github.com/tursodatabase/turso-mcp (tree: .claude-plugin/marketplace.json, claude-code/.mcp.json, claude-code/skills/turso/SKILL.md, codex/…, cursor/…) so install is '/plugin marketplace add xerj-org/xerj-mcp' + '/plugin install xerj@xerj' — the 10-line skill that `xerj init` already writes becomes SKILL.md in that repo.
- Add Turso's per-page re-orientation banner to every xerj.org/answers/*.md and /docs/*.md page, verbatim shape: '> ## Documentation Index\n> Fetch the complete documentation index at: https://xerj.org/llms.txt\n> Use this file to discover all available pages before exploring further.'
- Publish a discovery manifest at https://xerj.org/.well-known/mcp with Turso's keys ('name', 'description', 'endpoint', 'transport', 'authorization', 'documentation', 'openapi') so MCP-capable clients and crawlers find the server without reading prose.
- Write the bundled skill as an ordered decision procedure, copying the Turso SKILL.md heading pattern '## Which one to recommend (decide in this order)' with numbered branches that name the exact tool/flag (Turso: 'call `create_database` with `use_tursodb: true`'). For XERJ: 1. know the symbol → `xerj def`; 2. plain-English question → `xerj_search` MCP tool; 3. no node running → …; 4. no shell → HTTP-only path.
- On the field-report problem: Turso is evidence that a docs file with zero asks still ships a working product, so the ask's placement is the lever, not its volume. Do not file a 'required' item under a heading literally named '## Optional' (XERJ llms.txt line 223) — Turso's '## Optional' holds only Homepage/Discord/GitHub, which is what the llms.txt spec means by it. Move the report to its own '## Your side of the deal' section immediately after 'Start here', keep the one-command form ('xerj feedback --open-pr …'), and drop the 'owes'/'required, not optional' wording, which Turso's neutral catalog tone never uses.
