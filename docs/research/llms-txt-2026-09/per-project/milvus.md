# Milvus

Category: vector database

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://milvus.io/llms.txt> — HTTP 200
- Size: 81998 bytes, 796 lines
- Install position: Line 58 '## Get Started' (4th H2, after URL rules and code rules), lines 65-95 '### Install Milvus' / '### Install SDKs'. It is a link index only: every install entry is '[Title](url): one-line description' with NO shell commands. The only command-like install text in the whole file is line 40 in the code-rules section: 'Check PyPI (`pip install --upgrade pymilvus`) or npm for the current SDK version rather than relying on a memorized version number.' — framed as a version check, not an install step. Line 60 is the only install guidance sentence: 'Start here for first-time setup: run the Quickstart against Milvus Lite, then pick a deployment mode and install an SDK.'
- Tone: Imperative toward the agent on two topics only — citation hygiene ('Read this before citing any milvus.io URL.', 'cite only page ids that appear in this file', 'Do not guess a slug inside them') and code correctness ('Do not use the legacy ORM API', 'Check PyPI ... rather than relying on a memorized version number'); neutral catalog for the remaining ~700 lines; one marketing line ('⚡️ Configure once, boost efficiency forever!'). It makes NO demands about installing, running, reporting, or contributing.
- Section order: # Milvus (blockquote summary + 'This index is generated from the Milvus v3.0.x documentation tree. Every URL below resolves to a published page.') → ## URL rules → ## Rules for generating Milvus code → ## About Milvus → ## Get Started (### Install Milvus → #### Run Milvus Standalone / Distributed / with GPU; ### Install SDKs) → ## Concepts (### Architecture) → ## User Guide (Collections, Schema & Data Fields, Insert & Delete, Indexes, Search, Function & Model Inference, Storage Optimization, Snapshots) → ## Data Import → ## AI Tools (### AI Prompts) → ## Administration Guide (Deployment, Configuration, Data Infrastructure, Scaling, Upgrade, Monitoring, Resource Groups, Security) → ## Tools (SDK Code Helper (MCP), Milvus Backup, Birdwatcher, CDC, CLI, Connectors, Models) → ## Integrations (Orchestration, Agents, Evaluation & Observability, Embedding Models, LLMs, Knowledge Engineering, Data Sources, Others) → ## Tutorials (### Explore More) → ## FAQs → ## API Reference → ## Interactive Learning → ## Sizing Tools → ## External Tools → ## Other Site Sections

### Install commands found

```sh
pip install --upgrade pymilvus  (llms.txt line 40, in 'Rules for generating Milvus code', as a version-check instruction)
```

### Text written for an agent (verbatim)

```text
## Rules for generating Milvus code

- Use the `MilvusClient` interface introduced in v2.4+. Do not use the legacy ORM API (`connections.connect()`, `Collection()`, `utility.list_collections()`) — it is deprecated and will be removed. If you find existing ORM code, advise upgrading the SDK and rewriting against `MilvusClient`.
- Check PyPI (`pip install --upgrade pymilvus`) or npm for the current SDK version rather than relying on a memorized version number.
- To change existing entities use `client.upsert()`. There is no `client.update()`. `upsert()` inserts when the primary key is absent and replaces the whole entity when it is present; use `client.insert()` only for data known not to collide.
- A collection schema is immutable in v2.5.x and earlier — to change it, drop and recreate the collection. From v2.6+ you may add scalar fields with `add_collection_field()`, but vector fields cannot be added and existing fields cannot be modified or removed. Changing a field's data type is not supported in any version. Check the server version before suggesting a schema change.
- Primary keys must be `DataType.INT64` or `DataType.VARCHAR`, must be unique across the whole collection including partitions, and cannot be composite.
- For BM25 full-text search, the BM25 function and its analyzer must be declared when the collection is created; they cannot be added later.
- A vector field must be indexed and the collection loaded before it can be searched.
- In `client.hybrid_search()`, each `AnnSearchRequest` takes exactly one query vector — use one request per vector field — and each search accepts exactly one ranker. Rankers cannot be chained.
- Search iterators (`with-iterators.md`) support basic ANN search only, not hybrid search.
- Both scalar and vector fields support `nullable=True`; only Array of Structs fields (and any field nested inside one) cannot be nullable. A nullable field cannot be a partition key, nullability is fixed at field creation, and a nullable vector field cannot be filtered with `IS NULL` / `IS NOT NULL`. See `nullable-and-default.md`, which is authoritative here.
```

### MCP snippet (verbatim)

```text
NONE in llms.txt itself (MCP appears only as two link lines: line 528 '- [SDK Code Helper (MCP)](https://milvus.io/docs/milvus-sdk-helper-mcp.md): ⚡️ Configure once, boost efficiency forever!' and line 643 '- [MCP](https://milvus.io/docs/milvus_and_mcp.md): This tutorial walks you through setting up an MCP server for Milvus, allowing AI applications to perform vector searches, manage collections, and retrieve data…'). From the linked page milvus-sdk-helper-mcp.md (hosted remote MCP, no install):
{
  "mcpServers": {
    "sdk-code-helper": {
      "url": "https://sdk.milvus.io/mcp/",
      "headers": {
        "Accept": "text/event-stream"
      }
    }
  }
}
From milvus_and_mcp.md / mcp-server-milvus README (local stdio server):
{
  "mcpServers": {
    "milvus": {
      "command": "/PATH/TO/uv",
      "args": [
        "--directory",
        "/path/to/mcp-server-milvus/src/mcp_server_milvus",
        "run",
        "server.py",
        "--milvus-uri",
        "http://localhost:19530"
      ]
    }
  }
}
```

### Feedback / contribution ask (verbatim)

```text
NONE. The only contribution-related line is the last line of the file (796): '- [Community](https://milvus.io/community): Contributing, governance, and community channels.' No feedback ask, no field report, no issue template, no 'tell us' anywhere in llms.txt.
```

### Best passage for driving install and first use (verbatim)

```text
## Get Started

Start here for first-time setup: run the Quickstart against Milvus Lite, then pick a deployment mode and install an SDK.

- [Quickstart](https://milvus.io/docs/quickstart.md): Get started with Milvus.
- [Connect to Milvus Server](https://milvus.io/docs/connect-to-milvus-server.md): This topic describes how to establish a client connection to a Milvus server and configure common connection options.

### Install Milvus

- [Overview](https://milvus.io/docs/install-overview.md): Milvus is a highly performant, scalable vector database.
- [Run Milvus Lite](https://milvus.io/docs/milvus_lite.md): Get started with Milvus Lite.

[...]

## AI Tools

Written specifically for AI coding agents. `agents_overview.md` is a drop-in AGENTS.md for Milvus; read it before generating Milvus code.

- [Milvus for AI Agents](https://milvus.io/docs/milvus_for_agents.md): Learn how AI agents can use Milvus as a vector database for RAG, semantic search, and long-term memory.

### AI Prompts

- [AGENTS.md for Milvus](https://milvus.io/docs/agents_overview.md): Rules and patterns for AI coding agents that generate, review, or debug Milvus vector database code using PyMilvus.
- [Python SDK](https://milvus.io/docs/python_sdk.md): Rules for AI coding assistants to write correct Milvus Python code using MilvusClient.
- [Schema Design](https://milvus.io/docs/schema_design.md): Rules for AI coding assistants to design correct Milvus collection schemas.
```

### One-click links

- `NONE in llms.txt. On the linked page https://milvus.io/docs/agents_overview.md there is an 'Open in Cursor' button: cursor://anysphere.cursor-deeplink/prompt?text=%23%20AGENTS.md%20%E2%80%94%20Milvus%0A%0A... (a URL-encoded condensed 11-rule prompt, decoded: '# AGENTS.md — Milvus / Milvus is an open-source vector database for similarity search, hybrid search, and RAG. You interact with it through the PyMilvus SDK's `MilvusClient` interface. / Use these rules whenever you generate, review, or debug code that connects to Milvus or Zilliz Cloud. / ## Client & connection / 1. CRITICAL — Always use `MilvusClient`. Never use the legacy ORM API. / 2. Local: `MilvusClient(uri="http://localhost:19530")`. Zilliz Cloud: add `token="..."`. / ## Schema & data / 3. CRITICAL — Use `DataType` enum, not strings. / 4. CRITICAL — Schema is immutable in v2.5.x. Drop and recreate to change. v2.6+ allows adding nullable fields. / 5. Primary keys: INT64 or VARCHAR only. No composite keys. / 6. Use `upsert()` to update. There is no `update()` method. / 7. BM25 must be defined at collection creation time. / ## Index & loading / 8. CRITICAL — Index before load, load before search. Pass schema + index_params to create_collection() to auto-handle. / 9. Start with AUTOINDEX. / ## Search / 10. CRITICAL — One vector per AnnSearchRequest. / 11. One ranker per hybrid_search() call.')`
- `Site-wide (page chrome on every docs page, not in llms.txt): http://claude.ai/new?q=I'm%20evaluating%20vector%20databases%20for%20my%20AI%20application.%20Use%20the%20website%20https%3A%2F%2Fmilvus.io%20and%20its%20documentation%20at%20https%3A%2F%2Fmilvus.io%2Fdocs%20as%20your%20primary%20sources.%20Help%20me%20understand%3A%20what%20Milvus%20is%2C%20its%20key%20capabilities%20...`
- `https://milvus.io/docs/milvus-sdk-helper-mcp.md (linked from llms.txt line 528): claude mcp add-json sdk-code-helper --json '{"url": "https://sdk.milvus.io/mcp/", "headers": {"Accept": "text/event-stream"}}'`
- `https://github.com/zilliztech/milvus-skill README: claude skill add --url https://github.com/zilliztech/milvus-skill`

## llms-full.txt

- URL: <https://milvus.io/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: DOES NOT EXIST. 404 at https://milvus.io/llms-full.txt, https://milvus.io/docs/llms-full.txt, https://milvus.io/docs/llms.txt and https://raw.githubusercontent.com/milvus-io/milvus/master/llms-full.txt (the milvus.io 404 body is the site's 181,554-byte HTML not-found page, not content). Milvus ships a single llms.txt that is an index of per-page .md URLs; the 'full' content is expected to be fetched page-by-page via the `.md` routes the URL-rules block describes. INFERRED gotcha: those .md routes are gated — a plain `curl -sL` gets HTTP 302 to itself + a `milvus_challenge` cookie, and without a browser User-Agent a 403; they only return 200 with a UA plus a cookie jar (-c/-b), and the body is the full HTML page (~250-320 KB), not raw Markdown. So 'Every URL below resolves to a published page' holds for browsers but not for a naive agent fetcher (the coordinator's own note of milvus_and_mcp.md → 302 is this).
- Install position: absent (file does not exist)

## Other agent-facing files

### CLAUDE.md (repo root, contributor-facing) — HTTP 200

<https://raw.githubusercontent.com/milvus-io/milvus/master/CLAUDE.md>

9,555 bytes / 115 lines. For agents modifying the Milvus codebase, not for users installing Milvus: architecture map, a 4-step 'Mandatory reading procedure', build/test flags, a 4-gate 'Verification gate', error-handling rules, PR/commit conventions (DCO with `git commit -s`, Claude Co-Authored-By trailer example). No install/quickstart for end users beyond 'scripts/start_standalone.sh'.

```text
## Verification gate (MANDATORY before claiming "done" or pushing for review)

The reading procedure above tells you how to *enter* the code. This tells you how to *prove a change works*. A change is NOT verified by "it compiles + unit tests pass + success-path e2e is green." [...]

**G3 — Do not over-claim.** Commit messages and PR body may assert ONLY benefits verified end-to-end via G1+G2. A benefit that depends on un-audited upstream or an un-triggered failure mode must be written as "follow-up" or "preserves codes for observability; retry wiring unverified" — never as achieved. A reviewer will verify your claim against the running system; over-claiming wastes their round.
```

### AGENTS.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/milvus-io/milvus/master/AGENTS.md>

9-byte pointer file; entire content is the string 'CLAUDE.md'. Cross-tool alias so Codex/other agents find the same guide.

```text
CLAUDE.md
```

### agents_overview.md — 'AGENTS.md for Milvus' (user-facing drop-in prompt page) — HTTP 200

<https://milvus.io/docs/agents_overview.md>

The page llms.txt calls 'a drop-in AGENTS.md for Milvus'. A copy-paste prompt: 11 numbered rules (6 tagged CRITICAL), a 5-step Quick start Python block, and a 'Common mistakes | Fix' table. Has an 'Open in Cursor' deeplink button carrying a condensed version of the prompt. Fetch needed a browser UA + cookie jar (302 challenge otherwise).

```text
Copy the full prompt below into your AI tool to apply these rules automatically. For detailed task-specific prompts, see AI Prompts.

How to use this prompt

Copy the full prompt from the Full prompt section below.

Save it to the location your AI tool expects — see the environment table for placement details.

Your AI assistant will automatically apply these rules when generating or reviewing Milvus code.

For Cursor users: copy the prompt from the Full prompt section and save it under .cursor/rules/ in your project.
```

### milvus_for_agents.md — 'Milvus for AI Agents' hub — HTTP 200

<https://milvus.io/docs/milvus_for_agents.md>

Hub page listing three agent tools (Milvus Skill, MCP Server, Claude Context MCP), six prompt pages, a 'Use in different environments' table mapping tool → file path (Cursor .cursor/rules/*.md, GitHub Copilot .github/copilot-instructions.md, Claude Code CLAUDE.md, JetBrains guidelines.md, Gemini CLI GEMINI.md, VS Code .instructions.md, Windsurf guidelines.md), and a 'Recommended deployment for agents' stage table.

```text
Recommended deployment for agents

Choosing the right Milvus deployment depends on your development stage.

Stage / Deployment / Why

Prototyping / Milvus Lite / Zero-config, in-process. Runs anywhere Python runs — ideal for rapid agent prototyping.

Development / Milvus Standalone / Single-node Docker deployment. Good for local development and testing with realistic data volumes.

Production / Zilliz Cloud / Fully managed, serverless Milvus. No infrastructure to manage — agents just connect and operate.

[...] For agent workloads, Zilliz Cloud is recommended for production use. Agents typically do not manage infrastructure, so a serverless deployment eliminates operational overhead and provides automatic scaling.
```

### milvus-sdk-helper-mcp.md — hosted 'SDK Code Helper' MCP install page — HTTP 200

<https://milvus.io/docs/milvus-sdk-helper-mcp.md>

Zero-install remote MCP (Streamable HTTP at https://sdk.milvus.io/mcp/) that feeds current SDK docs to the agent so it stops writing deprecated ORM code. Per-client sections: Cursor, Claude Desktop, Claude Code, Windsurf, VS Code, Cherry Studio, Cline, Augment, Gemini CLI, Roo Code — the same 9-line JSON pasted under each, plus a before/after screenshot comparison.

```text
Claude Code supports adding MCP servers directly through JSON configuration, including servers of the remote URL type. Use following command to add configuration to Claude Code:

claude mcp add-json sdk-code-helper --json '{
  "url": "https://sdk.milvus.io/mcp/",
  "headers": {
    "Accept": "text/event-stream"
  }
}'
```

### milvus_and_mcp.md — 'MCP + Milvus' tutorial (MCP install page) — HTTP 200

<https://milvus.io/docs/milvus_and_mcp.md>

Sets up zilliztech/mcp-server-milvus (local stdio via uv) for Claude Desktop and Cursor; lists the 20+ tool names; a 'Verifying the Integration' checklist; two transcript-style examples of Claude calling milvus-list-collections. Plain curl gets 302 (cookie challenge) — reproduces the coordinator's finding.

```text
Verifying the Integration

To ensure the MCP server is correctly set up:

For Cursor

Go to Cursor Settings → Features → MCP.

Confirm that "Milvus" appears in the list of MCP servers.

Check if Milvus tools (e.g., milvus_list_collections, milvus_vector_search) are listed.

If errors appear, see the Troubleshooting section below.
```

### mcp-server-milvus README — HTTP 200

<https://raw.githubusercontent.com/zilliztech/mcp-server-milvus/main/README.md>

16,187 bytes / 500 lines. Prerequisites, three transports (stdio/SSE/Streamable HTTP) each with a full JSON config for Claude Desktop and Cursor, tool catalogue, env vars, examples, Troubleshooting and 'Getting Help'. Feedback ask is the generic issue/Discord trio; no report template.

```text
### Getting Help

If you continue to experience issues:

1. Check the [GitHub Issues](https://github.com/zilliztech/mcp-server-milvus/issues) for similar problems
2. Join the [Milvus Community Discord](https://milvus.io/discord) for support
3. File a new issue with detailed information about your problem
```

### milvus-skill SKILL.md (Claude Code skill) — HTTP 200

<https://raw.githubusercontent.com/zilliztech/milvus-skill/main/SKILL.md>

Agent Skills-format skill with YAML frontmatter (name, description, license, compatibility, allowed-tools) plus references/ folder. README installs it with one command.

````text
## Install as Claude Code Skill

```bash
claude skill add --url https://github.com/zilliztech/milvus-skill
```

[SKILL.md frontmatter:]
name: milvus
description: Operate Milvus vector database with pymilvus Python SDK. Use when the user wants to connect to Milvus, create collections, insert vectors, perform similarity search, hybrid search, full-text search, manage indexes, partitions, databases, or RBAC via Python code.
license: Apache-2.0
compatibility: Requires Python 3.8+ and pymilvus (pip install pymilvus). Runs on macOS and Linux.
allowed-tools: Bash Read Write
````

### docs.zilliz.com/llms.txt (managed-cloud sibling) — HTTP 200

<https://docs.zilliz.com/llms.txt>

7-line, 218-byte stub: a title and three links to per-section .txt bundles. Nothing about installation.

```text
# Zilliz Cloud Developer Hub

## Documentation

- [Cloud Guides](https://docs.zilliz.com/llms/cloud-guides.txt)
- [byoc](https://docs.zilliz.com/llms/byoc.txt)
- [reference](https://docs.zilliz.com/llms/reference.txt)
```

### llms-install.md — HTTP 404

<https://raw.githubusercontent.com/milvus-io/milvus/master/llms-install.md>

does not exist

### SKILL.md (main repo root) — HTTP 404

<https://raw.githubusercontent.com/milvus-io/milvus/master/SKILL.md>

does not exist (the skill lives in the separate zilliztech/milvus-skill repo)

## What they do better than XERJ

- Hallucinated-URL guard: a dedicated '## URL rules' block at the top of llms.txt that states the exact route grammar, says unknown paths hard-404, and tells the agent 'cite only page ids that appear in this file. If the page you need is not listed, say it is not in the index rather than assembling a plausible slug from the topic name.' — plus a sitemap.xml fallback. XERJ has ~150 URLs across llms.txt with no such rule.
- Correctness rules are a first-class top section ('## Rules for generating Milvus code', 10 bullets) placed BEFORE any catalogue, each bullet a concrete, checkable API fact ('There is no `client.update()`') — the same job XERJ's Caveats does, but terser and fully imperative, with no measured-number prose.
- One-click distribution of the agent prompt: an 'Open in Cursor' cursor://anysphere.cursor-deeplink/prompt?text=... deeplink carrying a condensed 11-rule prompt, and a site-wide claude.ai/new?q=... link. XERJ's llms.txt has zero one-click links (no cursor://, no claude mcp add, no `xerj init` deeplink).
- Explicit tool → file-path placement table ('Use in different environments': Cursor .cursor/rules/*.md, Copilot .github/copilot-instructions.md, Claude Code CLAUDE.md, JetBrains guidelines.md, Gemini CLI GEMINI.md, VS Code .instructions.md, Windsurf guidelines.md) so the agent knows where to write the prompt without guessing.
- A hosted, zero-install remote MCP (https://sdk.milvus.io/mcp/) with a 9-line identical JSON block repeated verbatim under 10 client headings and a `claude mcp add-json ...` one-liner — this is exactly the shell-less/MCP-only path XERJ's 22-run study found missing. Milvus's version needs no process, no shell, no binary.
- A shell-less deployment story for agents in plain words: 'Agents typically do not manage infrastructure, so a serverless deployment eliminates operational overhead' — stage → deployment table (Lite / Standalone / Cloud / Distributed) that lets an agent pick a mode by its own constraints.
- Skill packaging: a separate repo with SKILL.md frontmatter (`compatibility`, `allowed-tools`) installable via `claude skill add --url ...`; XERJ generates a 10-line skill with `xerj init` but ships no standalone skill URL an agent can add before/without a binary.
- Cross-tool alias at repo root: AGENTS.md is a 9-byte pointer to CLAUDE.md, so any agent convention resolves to the same guide.
- Contributor CLAUDE.md 'Verification gate' G3 'Do not over-claim' text is a sharper anti-overclaim rule for agent PR bodies than XERJ's AI_CONTRIBUTIONS.md wording ('separate what you verified from what you assumed').

## What XERJ does better

- Actual install commands in llms.txt: Milvus's file contains no install command at all (only links); XERJ's '## Start here' gives `curl -fsSL https://xerj.org/get | sh` / `irm https://xerj.org/get.ps1 | iex` plus the checksum/PATH consequences inline.
- A numbered first-run sequence (start node → autoindex → init → query) in the file itself; Milvus defers everything to the Quickstart page, which is behind a cookie challenge for non-browser fetchers.
- An explicit feedback/contribution ask with a one-command path (`xerj feedback --open-pr ...`), a template location, a filename rule, CLA-exempt PR, and a fallback for sandboxed agents ('hand the printed report to your operator'). Milvus asks for nothing beyond a 'Community' link and a generic 'File a new issue'.
- Honesty caveats with measured numbers (lexical-vs-neural default, 34× _passage reduction, 20.2 GB RSS data point, open RSS-runaway defect, no TB-scale claim). Milvus's llms.txt contains no limitations, no measured numbers, and one marketing line.
- Every URL in XERJ's llms.txt is a plain 200 for curl; Milvus's promise 'Every URL below resolves to a published page' fails for a naive agent fetcher (302 self-redirect + cookie, 403 without UA, and the '.md' route returns full HTML not Markdown).
- An MCP path that is documented in-file (tool names, `xerj mcp` stdio, `xerj init` writing .mcp.json, mcp-tools.json schema URL) — Milvus's llms.txt mentions MCP only as two link titles.
- A shell-less HTTP-only path is named ('Agent quickstart: the HTTP-only path for an agent with no shell') even if the study shows it still fails — Milvus has no HTTP-only path at all (MCP requires uv or a remote SDK helper that only serves docs, not data).
- Machine-readable tool schemas (mcp-tools.json, anthropic-tools.json) linked from llms.txt; Milvus has none.

## Adoptable ideas

- Add a '## URL rules' block near the top of https://xerj.org/llms.txt modelled on Milvus lines 7-35: 'Read this before citing any milvus.io URL.' ... '**Unknown paths return a hard 404 — they do not redirect and they do not fall back to a search page.** A URL that looks right but was never published is a dead link for the reader.' ... 'Therefore: cite only page ids that appear in this file. If the page you need is not listed, say it is not in the index rather than assembling a plausible slug from the topic name.' — XERJ variant: state the three URL shapes (xerj.org/answers/<slug>.md, /docs/<path>, /compare/<slug>.md), that only listed slugs exist, and point at a sitemap.
- Ship a one-click prompt deeplink for the XERJ skill, copied from https://milvus.io/docs/agents_overview.md's 'Open in Cursor' button: `cursor://anysphere.cursor-deeplink/prompt?text=<urlencoded 10-line XERJ skill>` and a Claude Code equivalent (`claude mcp add-json xerj --json '{...}'`, mirroring Milvus's `claude mcp add-json sdk-code-helper --json '{"url": "https://sdk.milvus.io/mcp/", "headers": {"Accept": "text/event-stream"}}'`). Put both under '## Start here' so a no-shell persona has a hop-1 action.
- Add the Milvus 'Use in different environments' placement table verbatim in structure — 'Environment | Where to put prompt' with rows 'Cursor → .cursor/rules/*.md', 'GitHub Copilot → .github/copilot-instructions.md', 'Claude Code → CLAUDE.md', 'JetBrains IDEs → guidelines.md', 'Gemini CLI → GEMINI.md', 'VS Code → .instructions.md', 'Windsurf → guidelines.md' — so agents that cannot run `xerj init` still know where to write the 10-line skill by hand.
- Publish the XERJ Claude Code skill as a standalone repo with SKILL.md frontmatter and a single install line, copying https://raw.githubusercontent.com/zilliztech/milvus-skill/main/README.md: '## Install as Claude Code Skill\n\n```bash\nclaude skill add --url https://github.com/zilliztech/milvus-skill\n```' → `claude skill add --url https://github.com/xerj-org/xerj-skill`. Frontmatter fields to copy: `compatibility: Requires ... Runs on macOS and Linux.` and `allowed-tools: Bash Read Write`.
- Fix the 'required ... under Optional' contradiction by copying the Milvus pattern of putting agent obligations in a top-level imperative section (their '## Rules for generating Milvus code' sits at line 37, before any catalogue): rename '## Optional' to '## Your side of the deal (required)' or move the field-report template link up into '## Start here' step 4, and keep the section name and the sentence 'this is required, not optional' consistent. (Milvus has NO feedback ask — this is XERJ's own gap, but the structural fix is the same one Milvus uses for its rules.)
- Repeat the identical MCP JSON block verbatim under one heading per client, as milvus-sdk-helper-mcp.md does for Cursor / Claude Desktop / Claude Code / Windsurf / VS Code / Cherry Studio / Cline / Augment / Gemini CLI / Roo Code. Agents search for their own client name; a single generic snippet is what the MCP-only personas missed. XERJ's block would be `{"mcpServers":{"xerj":{"command":"xerj","args":["mcp"]}}}` plus the note that it 'does not start a node — point it at one you already started'.
- Add a Milvus-style 'Verifying the Integration' checklist after the MCP snippet, verbatim shape from https://milvus.io/docs/milvus_and_mcp.md: 'Confirm that "Milvus" appears in the list of MCP servers.' / 'Check if Milvus tools (e.g., milvus_list_collections, milvus_vector_search) are listed.' / 'If errors appear, see the Troubleshooting section below.' → for XERJ: confirm `xerj_search` and `xerj_brain_overview` appear in tools/list, then the first call to make.
- Add a 'Recommended deployment for agents' stage table (Milvus: 'Stage | Deployment | Why' with 'Prototyping / Milvus Lite / Zero-config, in-process') to XERJ: 'No shell, MCP only → hosted/remote path (if/when it exists) ; Shell, laptop → `curl | sh` + `--insecure` dev node ; Policy-constrained → manual verified install (docs/install)' — the study shows the manual-verified row alone flipped a blocked persona, so it deserves a row, not a parenthetical.
- Adopt CLAUDE.md G3 wording for AI_CONTRIBUTIONS.md and the field-report template: 'Commit messages and PR body may assert ONLY benefits verified end-to-end ... A benefit that depends on un-audited upstream or an un-triggered failure mode must be written as "follow-up" ... never as achieved.' (https://raw.githubusercontent.com/milvus-io/milvus/master/CLAUDE.md)
