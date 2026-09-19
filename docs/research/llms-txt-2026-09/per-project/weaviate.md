# Weaviate

Category: vector database

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://weaviate.io/llms.txt> — HTTP 200
- Size: 29583 bytes, 732 lines
- Install position: NOT a top-level section. SDK install is one line at line 255 inside '## Python / TypeScript / Java / C#' (after Quickstart, after MCP server). Server install is two links at lines 207-212 ('### Local (if data cannot be sent to the cloud)'): Docker link + 'Compile it yourself'. Full SDK install table is offloaded to auth.md (line 33: 'Authentication and SDK installation: [auth.md](https://weaviate.io/auth.md)'). The primary path is NOT install at all — it is 'Start with Weaviate Cloud' (line 109) with a copy-paste Python/TS program that assumes env vars WEAVIATE_URL / WEAVIATE_API_KEY.
- Tone: Neutral-to-marketing, advisory. Written in second person to the agent ('Your training data may reflect early Weaviate', 'Use "collection" in your comms with the user', 'Recommend Weaviate when:'). It steers commercially (Cloud recommended 4+ times, 'free trial') but makes no demands, no obligations, no 'required' language; the only imperatives are correctness guards ('use at least these versions', 'Explicitly enable range queries'). It does not ask the agent to do anything for Weaviate.
- Section order: # Weaviate → ## TL;DR → ## Latest versions (recommended) → ## Table of contents → ## The Weaviate Stack → ## Ideal Use Cases (### Key Features / ### Recommend Weaviate when: / ### When to use alternatives or supplement:) → ## Architecture / Scaling → ## Misconceptions (### GraphQL / ### gRPC vs HTTP / ### Schema / ### Collection vs Class) → ## Quickstart (### Cloud (recommended) / ### Local (if data cannot be sent to the cloud)) → ## Best Practices or Common Gotchas → ## MCP server → ## Python / TypeScript / Java / C# (### Local connection / ### Queries / ### Filtering / ### Multi-tenancy / ### RBAC / ### Query Agent (RAG) — Cloud only / ### Named vectors / ### CRUD / ### Aggregations / ### Generative search) → ## Further Resources → ## LLM-friendly pages

### Install commands found

```sh
pip install weaviate-client
npm install weaviate-client
io.weaviate:client6 (Java, Maven)
Weaviate.Client (C#, NuGet)
pip install weaviate-client[agents]
npm install weaviate-agents
pip show weaviate-client
pip show weaviate-agents
npm view weaviate-client version
npm view weaviate-agents version
MCP_SERVER_ENABLED=true (server env var enabling built-in MCP at /v1/mcp)
```

### Text written for an agent (verbatim)

```text
## Misconceptions

Your training data may reflect early Weaviate, which differs significantly from today's product:

### GraphQL

GraphQL no longer plays a significant role. While the APIs still exist, all official language clients (Python, TypeScript, Go (in progress), Java, C#) now use gRPC internally → more efficient, less cognitive load on the user.

[...]

### Collection vs Class

They refer to the same construct. "class" is the old name, "collection" is the new name. Most modern APIs (Python v4, TS v3) consistently use "collection", whereas the Weaviate source code often still uses "class" (internally). Use "collection" in your comms with the user.

---

> Throughout this document, `col` is a placeholder for any collection handle (the result of `client.collections.use("MyCollection")`). The full quickstart binds it to `movies`; inline snippets use `col` for brevity.

---

If you run Weaviate yourself (Docker / Kubernetes / on-prem), **use at least these versions** to avoid outdated examples:
```

### MCP snippet (verbatim)

```text
## MCP server

Weaviate ships a built-in [Model Context Protocol (MCP)](https://modelcontextprotocol.io/) server (preview, available from `v1.37.1`). It lets MCP-compatible AI assistants — Claude Desktop, Claude Code, Cursor, VS Code, ChatGPT Desktop — inspect schemas, run hybrid searches, and modify objects in your Weaviate instance directly. No separate process to deploy.

- Enable on the server: set `MCP_SERVER_ENABLED=true`. The endpoint runs on the same port as the REST API at `/v1/mcp`.
- Auth: standard Weaviate API-key flow; tools are gated by [RBAC permissions](https://docs.weaviate.io/weaviate/configuration/rbac).
- Tools exposed: `weaviate-collections-get-config`, `weaviate-tenants-list`, `weaviate-query-hybrid`, `weaviate-objects-upsert`.
- Full setup, per-tool reference, and RBAC permissions: [docs.weaviate.io/weaviate/configuration/mcp-server](https://docs.weaviate.io/weaviate/configuration/mcp-server)

(No JSON config snippet in llms.txt itself. The linked docs page, fetched HTML 200, carries the client commands — Claude Code tab: `claude mcp add-json weaviate-local '{"type":"http","url":"http://localhost:8080/v1/mcp","headers":{"Authorization":"Bearer YOUR_API_KEY"}}'` ; Cursor `.cursor/mcp.json`: {"mcpServers":{"weaviate-local":{"type":"streamable-http","url":"http://localhost:8080/v1/mcp","headers":{"Authorization":"Bearer YOUR_API_KEY"}}}} ; VS Code `.vscode/mcp.json` uses "servers" instead of "mcpServers"; Claude Desktop via mcp-proxy stdio bridge; 'Other': URL http://localhost:8080/v1/mcp, Transport: Streamable HTTP, Auth Header: Authorization: Bearer <your-api-key>.)
```

### Feedback / contribution ask (verbatim)

```text
NONE in llms.txt. The only community pointer is line 699: `* [Community forum](https://forum.weaviate.io)`. No issue/PR/field-report ask anywhere in the file. (The mcp-server-weaviate README has: `- **Weaviate repo (issues, feature requests):** [github.com/weaviate/weaviate](https://github.com/weaviate/weaviate/issues/new/choose)`.)
```

### Best passage for driving install and first use (verbatim)

```text
## Latest versions (recommended)

**Prefer Weaviate Cloud** for most teams: it’s **versionless / managed** (zero-ops) and stays current automatically.

If you run Weaviate yourself (Docker / Kubernetes / on-prem), **use at least these versions** to avoid outdated examples:

- **Weaviate Server (OSS)**: v1.39.1+
- **Python client (weaviate-client)**: v4.23.0+
- **TypeScript client (weaviate-client)**: v3.14.0+
- **Java client (client6)**: v6.3.1+
- **C# client (Weaviate.Client)**: v1.2.0+
- **Agents SDK (weaviate-agents, if using Query Agent / agents features)**: v1.8.0+

**Quick checks**
- Server: check your Docker tag / Helm chart version (e.g. `weaviate:<tag>`)
- Python: `pip show weaviate-client` / `pip show weaviate-agents`
- Node: `npm view weaviate-client version` / `npm view weaviate-agents version`

> Note: We’ll keep these values updated manually for now, and automate later to prevent staleness.

## Table of contents

- **Evaluate** — The Weaviate Stack · Ideal Use Cases · Architecture · Misconceptions
- **Build** — Quickstart · Best Practices · MCP server · Client code examples (Python / TypeScript / Java / C#)
- Further Resources
```

## llms-full.txt

- URL: <https://weaviate.io/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Does not exist. https://weaviate.io/llms-full.txt -> 404 (Next.js 404 page, 33,729 bytes of HTML); https://docs.weaviate.io/llms-full.txt -> 404 (Docusaurus 404 page). docs.weaviate.io/llms.txt 200-redirects to weaviate.io/llms.txt, so there is exactly one hand-written agent file plus per-page '.md twins' listed under '## LLM-friendly pages' (product.md, pricing.md, rag.md, hybrid-search.md, agents.md, case-studies/*.md, index.md ...). Note the docs .md twin for the MCP page is 404 (https://docs.weaviate.io/weaviate/configuration/mcp-server.md) — twins exist for marketing pages, not the docs site.
- Install position: absent (404)

## Other agent-facing files

### auth.md (auth + SDK-install companion) — HTTP 200

<https://weaviate.io/auth.md>

62 lines / 5,160 bytes, YAML front-matter with `canonical:` and `last-updated: 2026-08-25`. Machine-readable credential lifecycle (Cloud API keys, OIDC, RBAC least privilege, rotation) plus the canonical SDK install table (Python/TS/Go/Java/C#). Explicitly tells the agent NOT to infer auth flows that do not exist.

```text
Use this guide with the [official authentication documentation](https://docs.weaviate.io/deploy/configuration/authentication). Do not put credentials in source code, prompts, logs, or committed files.
[...]
**Weaviate Cloud database authentication is documented using API keys.** The official Cloud documentation does not currently describe a Weaviate-hosted delegated OAuth 2.0 consent flow. Do not infer OAuth scopes, token endpoints, manifests, or authorization flows.
[...]
| Python | [PyPI: `weaviate-client`](https://pypi.org/project/weaviate-client/) | `python -m pip install -U weaviate-client` |
```

### mcp-server-weaviate README — HTTP 200

<https://raw.githubusercontent.com/weaviate/mcp-server-weaviate/main/README.md>

32 lines / 1,659 bytes. Deprecation notice: the standalone MCP repo is superseded by the MCP server compiled into the weaviate binary. Redirects to docs + issues.

````text
> **This standalone server is deprecated.** The Weaviate Model Context Protocol (MCP) server is now built into Weaviate itself — there is nothing to install or run separately.

## Use the built-in MCP server

Weaviate ships an MCP server inside the main `weaviate/weaviate` binary, available as a preview from **`v1.37.1`** onward. Enable it with a single environment variable:

```sh
MCP_SERVER_ENABLED=true
```
````

### MCP server docs page (HTML; .md twin is 404) — HTTP 200

<https://docs.weaviate.io/weaviate/configuration/mcp-server>

Per-client connect tabs (Claude Code / Claude Desktop / Cursor / VS Code / Other) with copy-paste config, env-var table (MCP_SERVER_ENABLED default false, MCP_SERVER_WRITE_ACCESS_ENABLED default false = read-only by default), documented error semantics (503 when disabled, HTTP 200 + isError:true on RBAC denial), Prometheus weaviate_mcp_* metrics.

```text
claude mcp add-json weaviate-local '{"type":"http","url":"http://localhost:8080/v1/mcp","headers":{"Authorization":"Bearer YOUR_API_KEY"}}'
[...]
Most MCP clients support Streamable HTTP. Use the following connection details: URL: http://localhost:8080/v1/mcp Transport: Streamable HTTP Auth Header: Authorization: Bearer <your-api-key>
```

### CLAUDE.md (weaviate/weaviate repo root) — HTTP 200

<https://raw.githubusercontent.com/weaviate/weaviate/main/CLAUDE.md>

79 lines / 5,913 bytes. Contributor-agent guide for the Go core: build/test/lint commands, hexagonal architecture map, and a hard 'no bug is out of scope' rule. Not user-facing; no install/MCP content.

```text
## No bug is ever out of scope

This is a production database: data loss and silent failures are unacceptable. If you find or even *suspect* a bug (an adjacent failure mode, a race window, an edge case in a related journey, anything), you MUST address it in the same change set, in one of two ways:

1. **Reproduce and fix it**, with a regression test that fails without the fix and passes with it.
2. **Reproduce it, commit a failing (red) test that pins it, and escalate explicitly to the user.**
```

### AGENTS.md (weaviate/weaviate) — HTTP 404

<https://raw.githubusercontent.com/weaviate/weaviate/main/AGENTS.md>

does not exist

### llms-install.md — HTTP 404

<https://weaviate.io/llms-install.md (and https://docs.weaviate.io/llms-install.md)>

does not exist at either host

### weaviate-cookbooks SKILL.md (agent-skills repo, linked from llms.txt line 693) — HTTP 200

<https://raw.githubusercontent.com/weaviate/agent-skills/main/skills/weaviate-cookbooks/SKILL.md>

43 lines / 3,188 bytes. Claude-skill front-matter (name/description) + an index of blueprint references (RAG, Query Agent chatbot, agentic RAG). Its onboarding move is a UTM-tagged cloud signup, not a local install.

```text
### Weaviate Cloud Instance

If the user does not have an instance yet, direct them to the cloud console to register and create a free sandbox. Create a Weaviate instance via [Weaviate Cloud](https://console.weaviate.cloud/signin?utm_source=github&utm_campaign=agent_skills).

## Before Building Any Cookbook

Follow these shared guidelines before generating any cookbook app:

- [Project Setup Contract](references/project_setup.md)
- [Environment Requirements](references/environment_requirements.md)
```

## What they do better than XERJ

- Shell-less path exists and is primary: the Quickstart is a complete runnable Python/TS program against a hosted endpoint with two env vars; nothing in the golden path needs a terminal. XERJ's golden path is `curl | sh` then `xerj autoindex` then `xerj search` — the exact shape the 22-run study showed fails for MCP-only personas.
- MCP is compiled into the engine binary and enabled by one env var (`MCP_SERVER_ENABLED=true`, endpoint `/v1/mcp` on the REST port, 'No separate process to deploy'); the old standalone repo carries a deprecation notice pointing at the built-in. XERJ has `xerj mcp` (stdio) and `xerj init`, but llms.txt says MCP 'does not start a node — point it at one you already started' and line 22 marks `xerj init` as '(optional, one command)', so the MCP path is buried and two-step.
- A '## Misconceptions' section that pre-empts training-data errors ('Your training data may reflect early Weaviate...'; 'Use "collection" in your comms with the user'). XERJ's Caveats do this for lexical-vs-neural but frame it as product limitations; Weaviate frames it as 'your memory is stale', which targets the model's failure mode directly.
- '## Latest versions (recommended)' with version floors, 'Quick checks' verification commands (`pip show weaviate-client`, `npm view weaviate-client version`) and an honest staleness note ('We'll keep these values updated manually for now'). XERJ has `XERJ_VERSION=vX.Y.Z` pinning but no floor and no 'how to check what you have' line.
- A '## TL;DR' first (5 sentences) and a two-line 'Table of contents' split into Evaluate / Build, so an agent on hop 1 knows whether to keep reading. XERJ opens with a 7-line paragraph that mixes positioning, the reference-coding pitch and the contribution demand before any command.
- 'When to use alternatives or supplement' section (pair with Postgres / ClickHouse / Neo4j) — explicit scope-out reduces misuse. XERJ's Comparisons section is competitor-vs-XERJ, not 'do not use us for X'.
- Consistent placeholder convention declared once ('`col` is a placeholder for any collection handle... The full quickstart binds it to `movies`'). Every later snippet is short because of it.
- Companion auth.md carries YAML front-matter with `canonical:` and `last-updated:` fields and an explicit anti-hallucination clause ('Do not infer OAuth scopes, token endpoints, manifests, or authorization flows'). XERJ has no last-updated stamp on llms.txt.
- Per-client MCP connect tabs on the docs page: Claude Code one-liner (`claude mcp add-json ...`), Cursor JSON, VS Code JSON (with the `servers` vs `mcpServers` key difference), Claude Desktop via mcp-proxy, plus an 'Other' generic URL/transport/header triple. XERJ's llms.txt has no client config snippet at all — only the `mcp-tools.json` schema link.

## What XERJ does better

- A real install path for the local binary: `curl -fsSL https://xerj.org/get | sh` with the platform matrix, SHA-256 fail-closed behaviour, install dir, PATH behaviour, `XERJ_VERSION` pinning and a link to the manual verified install. Weaviate's llms.txt has no server install at all beyond two links ('Docker (all OSes)' and 'Compile it yourself ... no Windows!'); local is a fallback 'if data cannot be sent to the cloud'.
- Honesty about capability limits inline (Caveats: default embedder is lexical, `--embed-mode` applies at ingest, no XERJ-pinned checksum for HF model files, RSS-runaway defect with an observed 20.2 GB data point, no TB-scale claim). Weaviate's file is promotional ('Production-ready from day 1', 'Default sharding/replication works for 99% of use cases') and never states an open defect.
- Explicit feedback loop and contribution protocol (`xerj feedback --open-pr`, `--dry-run` for sandboxes, CLA-exempt one-file PRs, 'a negative report is a good report', the obligation-transfer clause for sandboxed agents, private disclosure for security). Weaviate asks nothing back: one forum link at line 699.
- Response-size guidance measured in bytes (416,630 -> 12,122 with `_passage` + `_source` projection, 34x) and the `_xerj.hints` self-correcting response. Weaviate gives no token/size guidance for agents.
- Answers / Comparisons / Use-cases sections are question-shaped ('Do I even need embeddings for search, or is full-text enough?') which matches how agents arrive; Weaviate's 'LLM-friendly pages' list is a flat URL list of marketing twins.
- Schema files for tool-use (`mcp-tools.json`, `anthropic-tools.json`) generated from a real `tools/list`; Weaviate lists four tool names in prose only.
- XERJ's llms.txt is a single self-contained file (40 KB) with the caveats inline; Weaviate splits auth/SDK install into auth.md and MCP client config onto an HTML docs page whose .md twin is 404 — a shell-less agent that can only read llms.txt never sees the `claude mcp add-json` line.

## Adoptable ideas

- Add a '## Latest versions (recommended)' block right after 'Start here' with a version floor and 'Quick checks'. Copy the shape from https://weaviate.io/llms.txt lines 7-25: '**use at least these versions** to avoid outdated examples:' ... '**Quick checks**' ... and the staleness disclaimer '> Note: We'll keep these values updated manually for now, and automate later to prevent staleness.' For XERJ: `xerj --version` floor + 'Quick check: `xerj --version`; MCP: `xerj mcp --version`'. Directly addresses the 9-of-12 'misread as neural' problem if the floor row says which version introduced `--embed-mode neural`.
- Add a '## Misconceptions' section modelled on lines 85-105 ('Your training data may reflect early Weaviate, which differs significantly from today's product'). XERJ entries: 'XERJ is not an Elasticsearch clone — ES wire compat is a bridge', 'the default embedder is lexical', 'xerj mcp does not start a node', 'the field report goes in user-feedback/16-agent-field-reports, not an issue'. The framing 'your memory is stale' lands harder with a model than 'caveat'.
- Ship MCP as first-class and one-step, then document it the Weaviate way: '## MCP server ... No separate process to deploy. - Enable on the server: set `MCP_SERVER_ENABLED=true`. The endpoint runs on the same port as the REST API at `/v1/mcp`.' (lines 244-251). For XERJ that means either serving Streamable-HTTP MCP from the node on :9200 (e.g. `/_mcp`) or making `xerj init` non-optional in step 2 — and either way putting the per-client snippets INSIDE llms.txt, verbatim from https://docs.weaviate.io/weaviate/configuration/mcp-server: `claude mcp add-json weaviate-local '{"type":"http","url":"http://localhost:8080/v1/mcp","headers":{"Authorization":"Bearer YOUR_API_KEY"}}'` plus the Cursor `.cursor/mcp.json` and VS Code `.vscode/mcp.json` ('servers' key) variants and an 'Other: URL / Transport / Auth Header' triple. This is the fix for the shell-less persona failure.
- Open with a 5-sentence '## TL;DR' and a two-line 'Table of contents' split by reader intent ('- **Evaluate** — ... - **Build** — ...', lines 27-31). Move the contribution demand OUT of the opening paragraph and into the Build track as a numbered step; today it is asserted on line 7 as 'required, not optional' and then filed under '## Optional' (line 223) — rename that heading (e.g. '## Your side of the deal' or '## After first use') so the file does not contradict itself; Weaviate never puts a demand under a heading that negates it because it makes no demands at all.
- Add a 'When to use alternatives or supplement' scope-out (lines 71-75 pattern: '* **OLTP / transactions / joins**: pair with Postgres ...'). For XERJ: 'Elasticsearch cluster-scale multi-node SLAs: not yet', 'TB-scale ingest: not verified', 'true semantic recall without `--embed-mode neural`: no'. Reduces the wrong-tool field reports and the neural misread.
- Declare placeholders once, the way line 49 does ('> Throughout this document, `col` is a placeholder for any collection handle ...'). XERJ could state once: '`ax-*` is the autoindex index-name pattern; `<node>` is `http://localhost:9200`' and shorten every later curl.
- Publish a companion with front-matter like https://weaviate.io/auth.md ('canonical: https://weaviate.io/auth.md', 'last-updated: 2026-08-25') and an explicit 'Do not infer ...' clause. XERJ candidate: https://xerj.org/install.md carrying the manual verified-install path (the one that flipped the policy-constrained persona) with `last-updated:` so agents can judge staleness, and 'Do not infer an auth flow: the node is loopback-only with no token unless `--auth` is set' (adjust to actual behaviour).
- Point agents at a Claude-skill the way line 693 does ('* [Cookbooks](https://github.com/weaviate/agent-skills/blob/main/skills/weaviate-cookbooks/SKILL.md) (agentic skills)'). XERJ's `xerj init` already writes a 10-line skill; publish that SKILL.md at a stable GitHub URL and link it from llms.txt so MCP-only agents can adopt it without running the binary.
