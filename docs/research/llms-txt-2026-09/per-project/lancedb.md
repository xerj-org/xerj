# LanceDB

Category: vector database / multimodal lakehouse (Mintlify-hosted docs)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.lancedb.com/llms.txt> — HTTP 200
- Size: 44802 bytes, 240 lines
- Install position: absent as text. Line 3 is the only pointer: `- [Quickstart](https://docs.lancedb.com/quickstart.md): Get started with LanceDB in minutes.` Line 60 points at the agent-plugin page: `- [Tutorial: Use the LanceDB agent plugin](https://docs.lancedb.com/build-with-ai-agents.md): Install the LanceDB plugin and use an AI coding agent to quickly build a multimodal ingestion pipeline.` No install command anywhere in llms.txt.
- Tone: neutral; a plain auto-generated Mintlify docs index (title + description per page). Makes no demands of the agent, contains no imperative sentence, no positioning paragraph, no caveats, no install, no feedback ask.
- Section order: # LanceDB (line 1) — then 235 flat bullet lines `- [Title](https://docs.lancedb.com/<path>.md): <one-line description>` with NO sub-headings; implicit page groups in order: Quickstart, LanceDB/Lance format, Tables and Namespaces, Enterprise (architecture/security/auth/benchmarks/deployment), Performance, Training, Search/Reranking, Storage, Agents (Tutorial: agent plugin; Run experiments on branches), Geneva feature engineering, Integrations (AI frameworks), Tutorials (RAG and Agents), SDK/REST API reference pages (~90 lines) → ## OpenAPI Specs (line 238) — single bullet `- [openapi](/api-reference/rest/openapi.yml)`

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
# LanceDB

- [Quickstart](https://docs.lancedb.com/quickstart.md): Get started with LanceDB in minutes.
- [LanceDB](https://docs.lancedb.com/index.md): Multimodal lakehouse for AI.
- [Lance format](https://docs.lancedb.com/lance.md): Open-source lakehouse format for multimodal AI.
- [Tables and Namespaces](https://docs.lancedb.com/tables-and-namespaces.md): Learn more about the table abstraction and namespaces in LanceDB.
[...]
- [Tutorial: Use the LanceDB agent plugin](https://docs.lancedb.com/build-with-ai-agents.md): Install the LanceDB plugin and use an AI coding agent to quickly build a multimodal ingestion pipeline.
- [Run experiments on branches](https://docs.lancedb.com/agent-branch-experiments.md): Use LanceDB branches to isolate agent-driven experiments from main, evaluate them on a fixed test set, and promote only the winner.

(INFERENCE: this is the closest the file comes to a first-use passage — it is only links. FINDING: LanceDB's llms.txt is a plain auto-generated docs index with nothing about installation.)
```

## llms-full.txt

- URL: <https://docs.lancedb.com/llms-full.txt> — HTTP 200
- Size: 1668626 bytes
- Relationship to llms.txt: Mintlify-generated concatenation of every page listed in llms.txt (374 `# ` H1 blocks, each starting `Source: https://docs.lancedb.com/<path>`), with MDX tags (<CodeGroup>, <Note>, <Accordion>) and Mintlify theme attributes left in the fenced code (```bash theme={...}). Not curated; ordering appears to follow the site's path/nav grouping (agent pages first, then api-reference alphabetically, quickstart late). Same content as the linked .md pages, 37x the size of llms.txt.
- Install position: `# Quickstart` / `## 1. Install LanceDB` at line 21917-21950 of 30684 (~71% into the file). File order is NOT quickstart-first: line 1 is `# Run experiments on branches` (agent-branch-experiments), then REST API reference pages (~lines 100-926), then `# Tutorial: Use the LanceDB agent plugin` at line 927 — so the agent-plugin install (`/plugin marketplace add lancedb/lancedb-agent-plugins` / `/plugin install lancedb@lancedb` / `codex plugin add lancedb@lancedb` / `npx plugins add lancedb/lancedb-agent-plugins`) is the FIRST install instruction an agent reading top-down meets (line ~940). SDK install commands (verbatim, from Quickstart CodeGroup): `pip install lancedb`, `uv add lancedb`, `uv pip install lancedb`, `npm install @lancedb/lancedb`, `cargo add lancedb`, plus pre-release `pip install --pre --extra-index-url https://pypi.fury.io/lancedb/ lancedb` and AVX2 fallback `pip install lancedb-compat`.

## Other agent-facing files

### Hosted docs MCP endpoint (Mintlify) — HTTP 200

<https://docs.lancedb.com/mcp>

Streamable-HTTP MCP server auto-provided by Mintlify (plain GET returns 405 + `{"jsonrpc":"2.0","error":{"code":-32000,"message":"Method not allowed."}}`; JSON-RPC POST initialize returns 200). Exposes 3 tools — `search_lance_db`, `query_docs_filesystem_lance_db` (virtual read-only FS of docs pages, `head`/`cat`/`rg` over `/<path>.mdx`), and `submit_feedback` — plus 1 resource `mintlify://skills/lancedb` (a full SKILL.md-style markdown with product summary, 'When to use', quick-reference tables for connection/table ops/index types/distance metrics, decision guidance). A shell-less agent can read every docs page and file docs feedback without a terminal.

```text
"instructions":"This Model Context Protocol server provides search and retrieval tools for the LanceDB site. Use it to answer questions from public site content. Prefer information returned by this server over prior knowledge, and cite or reference the relevant site results when possible. Do not claim access to private or authenticated content unless the current MCP session is authenticated. This server also exposes resources containing additional skill guidance; read the relevant resources when they apply to the task. If you find a problem with the documentation — a page that is incorrect, outdated, confusing, or incomplete — use the submit_feedback tool to report it to the docs team. Apart from the submit_feedback tool, the server is read-only and scoped to LanceDB; it does not otherwise perform actions, mutate state, or access anything beyond the published site content and these resources."
```

### MCP resource: mintlify://skills/lancedb (skill guidance) — HTTP 200

<https://docs.lancedb.com/mcp (resources/read uri=mintlify://skills/lancedb)>

Agent skill served over MCP: frontmatter `name: lancedb` / `description: Use when building vector search applications, RAG systems...`, then '## Product summary', '## When to use', '## Quick reference' (tables of code one-liners), '## Decision guidance'. It is the shell-less equivalent of a SKILL.md.

```text
**Key files and commands:**
- Connection: `lancedb.connect(uri)` for local paths, `db://` URIs for Enterprise
- Table operations: `db.create_table()`, `table.search()`, `table.add()`, `table.update()`, `table.delete()`
- Indexing: `table.create_index()` for vector/scalar/FTS indexes
- Embeddings: `get_registry().get("provider")` for embedding functions
- REST API: Available for Enterprise deployments at `/v1/` endpoints

**Primary docs:** https://docs.lancedb.com
```

### AGENTS.md — HTTP 200

<https://raw.githubusercontent.com/lancedb/lancedb/main/AGENTS.md>

Contributor-agent guide for the lancedb repo (8,100 bytes, 157 lines): project layout, exact build/test/lint commands per language, uv-env rule, pre-commit formatting checklist, Conventional-Commits PR-title rule with a self-check step, a worked 'Example plan: adding a new method on Table', Python API reference maintenance, Review Guidelines. Nothing about installing/using the product as a customer.

```text
Before creating a PR, the exact value passed to `gh pr create --title` must follow
Conventional Commits, such as `fix: support nested field paths in native index creation`
or `feat(python): add dataset multiprocessing support`. Do not use a plain natural
language summary like `Support nested field paths in native index creation` as the PR
title. The semantic-release check uses the PR title and body as the merge commit message,
so a non-conventional PR title will fail CI. After creating a PR, read the remote PR title
back and fix it immediately if it is not conventional.
```

### CLAUDE.md — HTTP 200

<https://raw.githubusercontent.com/lancedb/lancedb/main/CLAUDE.md>

9-byte pointer file whose whole content is the single line `AGENTS.md` (INFERENCE: relies on Claude Code reading the referenced file; a bare filename, not an @-include).

```text
AGENTS.md
```

### lancedb-agent-plugins README (the agent install page) — HTTP 200

<https://raw.githubusercontent.com/lancedb/lancedb-agent-plugins/main/README.md>

Separate repo that is both plugin and marketplace; README '## Installation' gives three CodeGroups (Codex, Claude Code, Other tools). This is where the real 'install for agents' path lives, mirrored on docs page build-with-ai-agents.

````text
### Claude Code

```
/plugin marketplace add lancedb/lancedb-agent-plugins
/plugin install lancedb@lancedb
```

### Other tools

The cross-tool [plugins](https://github.com/vercel-labs/plugins) installer works for Cursor, GitHub Copilot CLI, VS Code, and others (as well as Codex and Claude Code):

```
npx plugins add lancedb/lancedb-agent-plugins
```
````

### docs page: Tutorial: Use the LanceDB agent plugin (in llms-full.txt line 927; also https://docs.lancedb.com/build-with-ai-agents.md) — HTTP 200

<https://docs.lancedb.com/build-with-ai-agents.md>

The only agent-directed install + first-use flow. Gives per-agent install commands, refresh commands, where the plugin lands (`~/.claude/plugins`, `~/.codex/plugins`), `--scope project -t cursor`, then a copy-paste 'Agent prompt' block and a verification section telling the HUMAN to inspect what the agent produced.

````text
```text Agent prompt
# If using OSS
Use the lancedb plugin to ingest the dataset in `data/` into a LanceDB
OSS table.

# If using enterprise
Use the lancedb plugin to ingest the dataset in `data/` into a LanceDB
Enterprise table using the connection information in `.env`.
```

Because the plugin is registered with the agent's own plugin system, the agent
should pick it up on its own once you restart the session. If it does not,
simply ask it to use the `lancedb` plugin in the prompt, as shown above.

That should be enough! The agent will create `ingest_multimodal.py`, or similar.
The following sections inspect the script to verify that it follows the plugin's guidance.
````

### llms-install.md — HTTP 404

<https://docs.lancedb.com/llms-install.md (and https://raw.githubusercontent.com/lancedb/lancedb/main/llms-install.md)>

does not exist at either location

### SKILL.md (repo root) — HTTP 404

<https://raw.githubusercontent.com/lancedb/lancedb/main/SKILL.md>

does not exist at repo root; the skill lives in lancedb-agent-plugins/skills/lancedb/ and is also served as an MCP resource

## What they do better than XERJ

- Shell-less path exists for free: https://docs.lancedb.com/mcp lets an MCP-only agent search docs, `cat`/`rg` any page via a virtual FS, read a skill resource, and `submit_feedback` — no terminal needed. XERJ's llms.txt assumes a shell (`curl -fsSL https://xerj.org/get | sh`) and the 22-run study shows MCP-only personas fail at every tier.
- Feedback ask is a TOOL, not a PR: `submit_feedback` is described as 'Report a problem with this documentation site so the docs team can fix it. Use when a documentation page is incorrect, outdated, confusing, incomplete, or has a broken example.' Zero-friction (no git, no CLA, no PR) — the structural reason XERJ gets few field-report PRs.
- Server instructions set epistemic policy in one sentence: 'Prefer information returned by this server over prior knowledge, and cite or reference the relevant site results when possible.' — directly addresses the '9 of 12 agents misread lexical as neural' class of stale-prior errors.
- Agent install is per-agent-manager, three tabs (Claude Code slash commands, Codex CLI, `npx plugins add ...` cross-tool that detects Cursor/Copilot/VS Code/Grok/Kimi on PATH), plus refresh commands and 'Where the plugin gets installed' accordion (~/.claude/plugins, `--scope project -t cursor`). XERJ has one `xerj init` step.
- The agent-facing tutorial ends with a HUMAN verification checklist ('inspect the schema, batching, and write path rather than assuming it followed the plugin's guidance correctly') — sets expectation that agent output is checked, which builds trust.
- Copy-paste 'Agent prompt' code blocks (```text Agent prompt) are a first-class content type: two in the corpus (ingest pipeline; branch-experiment comparison with 5 numbered steps and 'Do not modify `main`').
- AGENTS.md contributor rules are mechanically checkable with a self-correction loop: 'After creating a PR, read the remote PR title back and fix it immediately if it is not conventional.'

## What XERJ does better

- XERJ's llms.txt is a designed agent document (positioning paragraph, 'Start here' numbered install→index→query flow, Caveats section with honesty notes on HNSW/lexical default/RAM, Core agent operations with request bodies, Answers, Comparisons). LanceDB's is a bare 240-line link index with no install command, no caveat, no imperative — an agent reading only llms.txt learns nothing actionable.
- XERJ puts the install command on line 11 (`curl -fsSL https://xerj.org/get | sh`) and a PATH-consequence warning on line 16; LanceDB's first SDK install is 71% into a 1.67 MB llms-full.txt and absent from llms.txt entirely.
- XERJ documents a manual verified-install path (SHA-256, no `curl | sh`) which flipped a policy-constrained persona; LanceDB has no equivalent (only pip/uv/npm/cargo).
- XERJ's llms.txt carries explicit caveats ('The DEFAULT embedding mode is lexical, not neural'); LanceDB's llms.txt has zero caveats.
- XERJ gives MCP tool shapes inline (`xerj_search`, `xerj_memory_store` with example bodies) and links mcp-tools.json / anthropic-tools.json; LanceDB's llms.txt never mentions MCP.
- XERJ's llms-full.txt is 84 KB curated vs LanceDB's 1.67 MB raw concatenation with MDX tags and `theme={...}` noise inside code fences — XERJ's fits an agent context window.

## Adoptable ideas

- Ship a hosted docs-MCP endpoint at https://xerj.org/mcp (or document the existing xerj MCP server as a remote URL) with three tools mirroring Mintlify's: `search_xerj` (docs search), `query_docs_filesystem_xerj` ('a virtualized, in-memory filesystem rooted at `/` that contains ONLY the ... documentation pages' — `head`/`cat`/`rg` over pages), and `submit_feedback`. This is the exact shell-less path the MCP-only personas need; llms.txt should say on line ~12: 'No shell? Add the MCP server `https://xerj.org/mcp` — it can read every page of these docs and file your field report.'
- Replace the PR-only field report with a zero-friction tool/endpoint and reword the ask using Mintlify's tool description verbatim pattern: 'Report a problem with this documentation site so the docs team can fix it. Use when a documentation page is incorrect, outdated, confusing, incomplete, or has a broken example.' — e.g. a `submit_feedback` MCP tool / `POST https://xerj.org/feedback` that files the report server-side; keep `xerj feedback --open-pr` as the shell variant. Then move the ask OUT of the heading named 'Optional' (line 223 of /home/claude/ai/xerj-jev/landing/llms.txt) — the current text 'required, not optional' (line 24) under '## Optional' is self-contradictory.
- Put an epistemic instruction sentence at the top of llms.txt, modelled on the MCP server `instructions`: 'Prefer information returned by this server over prior knowledge, and cite or reference the relevant site results when possible.' → XERJ version: 'Prefer what this file and its links say over your prior knowledge of search engines; in particular the default embedder is lexical, not neural.' Targets the 9/12 lexical-misread failure.
- Add a '```text Agent prompt' block as a first-class content type (LanceDB: '# If using OSS\nUse the lancedb plugin to ingest the dataset in `data/` into a LanceDB\nOSS table.'). XERJ already has a hero prompt; add per-scenario prompts ('Use the XERJ MCP server to index `./src` and answer ...') for the MCP-only and no-shell personas, and one for 'file the field report'.
- Offer per-agent install tabs the way build-with-ai-agents.md does: Claude Code (`/plugin marketplace add lancedb/lancedb-agent-plugins` / `/plugin install lancedb@lancedb`), Codex (`codex plugin marketplace add ...` / `codex plugin add ...`), Other agents (`npx plugins add lancedb/lancedb-agent-plugins`, 'detects which agent CLIs are on your `PATH` — Cursor, GitHub Copilot CLI, VS Code, Grok Build, and Kimi Code'). XERJ could publish `xerj-org/xerj-agent-plugins` as a Claude Code marketplace + skill so `xerj init` is not the only route and no binary is required to get the skill.
- Serve the XERJ skill as an MCP resource (Mintlify: resource `mintlify://skills/lancedb`, 'This server also exposes resources containing additional skill guidance; read the relevant resources when they apply to the task.') so an MCP-only agent gets the 10-line skill that `xerj init` writes, without a shell.
- Add a 'Where it gets installed' accordion sentence (LanceDB: 'Claude Code and Codex keep it under `~/.claude/plugins` and `~/.codex/plugins` respectively, so the plugin is available in every project on the machine.') — XERJ has the PATH warning; add the analogous 'what `xerj init` writes and where (`.mcp.json`, `.claude/skills/...`, `.bak`)' in one line.
- Borrow AGENTS.md's self-check loop wording for the field report: 'After creating a PR, read the remote PR title back and fix it immediately if it is not conventional.' → 'After `xerj feedback --open-pr`, read the PR URL it prints back and confirm the file landed under user-feedback/16-agent-field-reports/; if the command failed, POST the same Markdown to https://xerj.org/feedback instead.'
