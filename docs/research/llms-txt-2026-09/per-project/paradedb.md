# ParadeDB

Category: search engine (Postgres extension)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://www.paradedb.com/docs/llms.txt (docs.paradedb.com/llms.txt 301-redirects here)> — HTTP 200
- Size: 18336 bytes, 131 lines
- Install position: line 5 of 131 (3rd bullet) — a link only, no command in the file: '- [Install ParadeDB](https://www.paradedb.com/docs/start/install.md): How to run the ParadeDB Docker image'. The actual install commands live behind that link.
- Tone: Neutral, catalogue. Makes zero demands of the agent inside llms.txt — it is a plain auto-generated Mintlify docs index with nothing about installation beyond a one-line link. The only imperative agent-facing text is the per-page 'Fetch the complete documentation index ... before exploring further' banner and the SKILL.md (imperative: 'always fetch', 'Do **not** use any tool other than', 'Never silently switch to guessed documentation structure').
- Section order: # ParadeDB (single H1; no H2 sections — a flat bullet list of 129 [Title](url.md): description links, Mintlify-generated) → Implicit order of the list: Start (Welcome, vs. Alternatives, Install, Connect Your App, Create Your First Index, Run Queries, Load Data, Integrate with AI, Go to Production) → Reference (indexing, tokenizers, token filters, full-text queries, filtering, vector, hybrid, aggregates, joins, operators, configuration, SQL functions) → Concepts (Architecture, Guarantees, How the Index Works, text/vector/hybrid, Limitations) → Operate (deploy: extension/Kubernetes/Railway/Render/Fly/DigitalOcean/Dokku, logical replication, HA, BYOC, Enterprise, maintenance, tuning, upgrading, CI) → Project (Roadmap, Changelog x5, Help and Support)

### Install commands found

```sh
(none in llms.txt itself)
From linked install.md: docker run \
  --name paradedb \
  -e POSTGRES_USER=myuser \
  -e POSTGRES_PASSWORD=mypassword \
  -e POSTGRES_DB=mydatabase \
  -v paradedb_data:/var/lib/postgresql/ \
  -p 5432:5432 \
  -d \
  paradedb/paradedb:latest
From linked install.md: docker exec -it paradedb psql -U myuser -d mydatabase -W
From linked ai-agents.md: npx skills add paradedb/agent-skills
```

### Text written for an agent (verbatim)

```text
(none in llms.txt itself.) Every linked .md page, however, opens with this agent-directed banner:
> ## Documentation Index
> Fetch the complete documentation index at: https://www.paradedb.com/docs/llms.txt
> Use this file to discover all available pages before exploring further.
```

### MCP snippet (verbatim)

````text
(none in llms.txt.) From ai-agents.md:
**MCP Endpoint:**

```text theme={null}
https://www.paradedb.com/docs/mcp
```

This allows MCP-enabled tools to query ParadeDB documentation programmatically and provide contextual assistance.

(Verified: GET https://www.paradedb.com/docs/mcp and https://www.paradedb.com/mcp both return HTTP 405 — protocol endpoint, POST-only. No JSON config block is published anywhere; the endpoint URL is the whole snippet.)
````

### Feedback / contribution ask (verbatim)

```text
(none in llms.txt.) Linked support.md says only:
For questions regarding enterprise support or commercial licensing, please [contact sales](mailto:sales@paradedb.com).
For community support and general questions, please join the [ParadeDB Community Slack](https://www.paradedb.com/slack).

## Ask a Question

Use the **"Ask a question..."** bar at the bottom of any page to get instant answers about ParadeDB.

There is NO ask for field reports, PRs, issues, or contributions anywhere in llms.txt, llms-full.txt, the ai-agents page, or SKILL.md.
```

### Best passage for driving install and first use (verbatim)

````text
# Integrate with AI

> Teach your coding assistant to use ParadeDB

Before getting started, let's give your coding agent full context of ParadeDB by adding the ParadeDB agent skill.

```bash theme={null}
npx skills add paradedb/agent-skills
```

This installs `paradedb-skill` into your agent's skills directory (for example, Codex uses `$CODEX_HOME/skills/paradedb-skill`) and works with all major
coding assistants like Claude Code, Cursor, Codex, Windsurf, Gemini, and more.

For manual and tool-specific setup instructions, see the [agent-skills repository](https://github.com/paradedb/agent-skills).

(Source: https://www.paradedb.com/docs/start/ai-agents.md, HTTP 200; also appears verbatim at line 13291 of llms-full.txt)
````

### One-click links

- `(none in llms.txt)`
- `From ai-agents.md: npx skills add paradedb/agent-skills`
- `From ai-agents.md: Cursor marketplace plugin https://cursor.com/marketplace/parade-db`

## llms-full.txt

- URL: <https://www.paradedb.com/docs/llms-full.txt (docs.paradedb.com/llms-full.txt redirects here)> — HTTP 200
- Size: 680006 bytes
- Relationship to llms.txt: Concatenation of every page listed in llms.txt, each as '# Title / Source: <url> / description / body' (Mintlify-generated; MDX components like <ArchitectureDiagram /> leak through as literal tags). Page order appears to be filesystem/alphabetical-by-directory (concepts, operate/deploy, reference/aggregates, ...), not the curated nav order of llms.txt. Separate: the marketing site root https://www.paradedb.com/llms.txt (200, 8323 B, hand-written: '## Website', '## When to use ParadeDB', '## Availability and pricing', '## Blog', '## Customers', '## Learn') and https://www.paradedb.com/llms-full.txt (200, 444921 B, 6614 lines: blog + customers + learn articles, not docs).
- Install position: line 14253 of 18613 ('# Install ParadeDB'), docker run at line 14266 — i.e. 77% of the way through the file. The file is NOT in reading order: it opens with '# Architecture' (concepts), and '# Welcome to ParadeDB' is at line 14292, after Install. '# Integrate with AI' is at line 13291. A first-time agent reading top-down gets 13k lines of aggregates/tokenizer reference before any install command.

## Other agent-facing files

### Site-root llms.txt (hand-curated, distinct from docs index) — HTTP 200

<https://www.paradedb.com/llms.txt>

Marketing-site LLM index: positioning, when-to-use, pricing tiers, blog/customer/learn links, MCP pointer; explicitly disclaims being a hosted DB API

```text
> Append `.md` to any blog, customer, or learn URL (e.g. `https://www.paradedb.com/blog/<slug>.md`) to fetch its Markdown source.
> An MCP server is available at `https://www.paradedb.com/mcp` (streamable HTTP) with tools to search and read this content.
[...]
This website and its MCP server provide product information, blog posts, customer stories, and learn articles, not a hosted database API.
```

### Integrate with AI (docs page = the agent install page) — HTTP 200

<https://www.paradedb.com/docs/start/ai-agents.md>

One-command skill install for all coding agents + docs-MCP endpoint + Cursor plugin

```text
npx skills add paradedb/agent-skills
```

### agent-skills README.md — HTTP 200

<https://raw.githubusercontent.com/paradedb/agent-skills/main/README.md>

Skill repo: one-line install, manual fallback table per agent (global/project dirs), curl|tar install, rationale for fetch-live-docs design

```text
Instead of bundling static docs that can become stale, this skill instructs agents to fetch the latest ParadeDB docs to answer your questions.
The skill includes a tiny script called `scripts/paradedb-docs` that allows the agent to fetch only the documentation using `curl`. This approach ensures the agent sees the
full content of docs instead of a summarized view and makes it easy to allow the agent to fetch the docs freely without also granting it unrestricted access to `curl`.
```

### SKILL.md (agent-skills repo root) — HTTP 200

<https://raw.githubusercontent.com/paradedb/agent-skills/main/SKILL.md>

The real agent-facing doc: frontmatter trigger description, fetch-docs-via-script protocol, 6 response guidelines (version check SQL, naming corrections), mandatory network-failure rules

```text
## Network Failure Rules (Mandatory)

If any documentation cannot be fetched due to DNS/network/access errors:

1. State clearly that live docs could not be accessed and include the actual error.
2. If you have cached session docs from an earlier successful fetch, say that
   you can continue from that cached copy unless the user wants to stop.
3. If you do not have cached session docs, ask whether to proceed with
   local/repo-only context or to retry later.
4. Do **not** invent or infer doc URLs, page paths, or feature availability.
5. Do **not** present unverified links as real.
6. Label any fallback statements as assumptions and keep them minimal.

Never silently switch to guessed documentation structure when network access fails.
```

### EXAMPLES.md (agent-skills repo) — HTTP 200

<https://raw.githubusercontent.com/paradedb/agent-skills/main/EXAMPLES.md>

Categorized example prompts a user can paste to their agent after installing the skill

````text
## Getting Started & Setup

```text
What are the ways to index large datasets for search using ParadeDB?

What's the quickest way to create a full-text searchable table?
```
````

### MCP endpoints — HTTP 405

<https://www.paradedb.com/docs/mcp and https://www.paradedb.com/mcp>

Streamable-HTTP MCP servers (docs search; site content search). GET returns 405 — protocol-only, no human page, no published client-config JSON

```text
(README note) The `/mcp` route is a protocol endpoint, not a human-readable docs page.
```

### AGENTS.md (paradedb/paradedb) — HTTP 404

<https://raw.githubusercontent.com/paradedb/paradedb/main/AGENTS.md>

does not exist

### CLAUDE.md (paradedb/paradedb) — HTTP 404

<https://raw.githubusercontent.com/paradedb/paradedb/main/CLAUDE.md>

does not exist

### llms-install.md — HTTP 404

<https://www.paradedb.com/docs/llms-install.md>

does not exist

### repo-root llms.txt / .cursorrules (paradedb/paradedb) — HTTP 404

<https://raw.githubusercontent.com/paradedb/paradedb/main/llms.txt>

does not exist (also .cursorrules 404)

## What they do better than XERJ

- Agent onboarding is ONE command that works across 76 agents: `npx skills add paradedb/agent-skills` — no shell-specific PATH caveats, no 'which agent are you' branching in the doc; the branching lives in the installer and a fallback table in the README. XERJ's `xerj init` needs the binary installed and on PATH first.
- Correction to the brief: they DO have an agent layer, it is just not in llms.txt. The SKILL.md is a tight ~100-line agent contract with (a) a frontmatter `description` that makes the skill self-triggering, (b) a version-probe instruction ('check first with `SELECT extversion FROM pg_extension WHERE extname = 'pg_search';`'), (c) naming-drift corrections (USING paradedb not USING bm25), (d) 'Mandatory' network-failure rules forbidding invented URLs. XERJ's 10-line generated skill is much thinner.
- The live-docs-via-restricted-script pattern: 'fetch the docs freely without also granting it unrestricted access to `curl`' — solves the permission-prompt problem for sandboxed agents by shipping a 5-line script whose only allowed argument is a .md/.txt path.
- Every .md page carries the same 3-line 'Documentation Index ... fetch ... before exploring further' banner, so an agent landing on any deep page is routed back to the index. XERJ pages do not self-route.
- Two-tier llms.txt: hand-written site-root file for positioning/pricing/when-to-use, machine-generated docs file for reference. The root file explicitly tells agents what the MCP server is NOT ('not a hosted database API'), preventing a class of misuse.
- Docs MCP server is published and reachable (streamable HTTP, no local process) — exactly the shell-less-persona path XERJ's study found missing. Cursor users get a marketplace plugin.
- llms.txt is short (131 lines / 18 KB) and pure index — each line is title + one-sentence description, no inline prose, so it costs an agent almost nothing to scan.
- Tone: makes no demands and creates no obligations. A first-time agent is never told it 'owes' anything.

## What XERJ does better

- XERJ's llms.txt contains the actual install commands in-file at line 11-12 (`curl -fsSL https://xerj.org/get | sh` / `irm https://xerj.org/get.ps1 | iex`) plus a numbered first-run sequence; ParadeDB's llms.txt has only a link and its llms-full.txt buries `docker run` at line 14266 of 18613.
- XERJ's llms-full.txt is curated and ordered for reading (1,235 lines / 84 KB); ParadeDB's is a 680 KB alphabetical concatenation that opens with '# Architecture' and puts '# Welcome' after '# Install' — a top-down reader hits 13k lines of aggregate reference first, and MDX tags (`<ArchitectureDiagram />`, `theme={null}`) leak through.
- XERJ has a 'Caveats (read before you rely on a capability)' section with honest-claims rules (lexical default, measured recall, RSS sizing); ParadeDB's llms.txt carries no caveats at all — 'beta' vector search is only disclosed inside SKILL.md guideline 6.
- XERJ exposes ready-made tool schemas (mcp-tools.json / openai-tools.json / anthropic-tools.json) and a documented HTTP-only path for shell-less agents; ParadeDB publishes only an MCP URL with no config snippet.
- XERJ documents the PATH consequence, exit codes, progress lines, and an estimate-first protocol for running on a human's machine; ParadeDB assumes Docker and a human at psql.
- XERJ has an explicit feedback/contribution channel with a one-command PR (`xerj feedback --open-pr`) and a defect-triage rule (issue vs PR vs nothing); ParadeDB has zero ask (Slack + sales only).

## Adoptable ideas

- Ship the agent skill on the vercel-labs skills registry so onboarding is one universal command. Wording to copy (https://www.paradedb.com/docs/start/ai-agents.md): 'Before getting started, let's give your coding agent full context of XERJ by adding the XERJ agent skill.' + '```bash\nnpx skills add xerj-org/agent-skills\n```' + 'works with all major coding assistants like Claude Code, Cursor, Codex, Windsurf, Gemini, and more.' This directly addresses the shell-less/policy-constrained personas: the skill installs without XERJ being on PATH, and can carry the manual-verified-install path inside it.
- Put a 3-line self-routing banner at the top of EVERY xerj.org/*.md page and llms-full.txt, verbatim pattern from ParadeDB: '> ## Documentation Index\n> Fetch the complete documentation index at: https://xerj.org/llms.txt\n> Use this file to discover all available pages before exploring further.'
- Add a 'Network Failure Rules (Mandatory)' block to XERJ's generated skill and llms.txt Caveats, adapted from https://raw.githubusercontent.com/paradedb/agent-skills/main/SKILL.md: '4. Do **not** invent or infer doc URLs, page paths, or feature availability.\n5. Do **not** present unverified links as real.' — pairs with XERJ's existing 'never invent a number' rule for progress lines.
- Add a version-probe-before-answering instruction like SKILL.md guideline 2 ('State ParadeDB/Postgres version assumptions when syntax may differ ... check first with `SELECT extversion ...`'). XERJ equivalent: 'check `xerj --version` / `GET /` before relying on a flag that is release-gated (e.g. `--max-minutes`, `_passage`)' — this would have caught the 9/12 'misread lexical as neural' agents if the probe also printed embed-mode.
- Ship a restricted docs-fetch script in the skill, copying the README rationale (https://raw.githubusercontent.com/paradedb/agent-skills/main/README.md): 'makes it easy to allow the agent to fetch the docs freely without also granting it unrestricted access to `curl`.' A `scripts/xerj-docs <path>.md` that only hits xerj.org lets sandboxed agents read llms.txt without a blanket curl permission.
- Fix the 'required ... under Optional' contradiction by inverting ParadeDB's lesson: ParadeDB asks for nothing and gets nothing; XERJ asks in prose and files the ask under '## Optional'. Rename the section (e.g. '## Your side of the deal') and move the one-command `xerj feedback --open-pr` line INTO the numbered first-run list as its own step with a hard trigger ('before you end the session'), not as a duplicate step 4. ParadeDB shows a numbered Start list (Install → Connect → First Index → Run Queries → Integrate with AI → Go to Production) is what agents follow; a step that is in the list gets done, a link under Optional does not.
- Publish a hosted, no-process docs-MCP endpoint and a one-line Cursor plugin like https://cursor.com/marketplace/parade-db, and say in llms.txt what it is NOT, copying the root-file disclaimer style: 'This website and its MCP server provide product information ..., not a hosted database API.' XERJ's equivalent: '`xerj mcp` ... does not start a node — point it at one you already started' already exists; make it a top-of-file statement, not a trailing note under Optional.
- Add an EXAMPLES.md of paste-able prompts, grouped like https://raw.githubusercontent.com/paradedb/agent-skills/main/EXAMPLES.md ('## Getting Started & Setup', '## Indexing', '## Basic Full-Text Search'), so a human operator can hand an agent a first task that exercises install → autoindex → search → feedback in one prompt.
