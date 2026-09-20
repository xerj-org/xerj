# SurrealDB

Category: multi-model database / "context layer for AI agents"

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://surrealdb.com/llms.txt> — HTTP 200
- Size: 27938 bytes, 294 lines
- Install position: First runnable command is line 65 (end of '### How to call SurrealDB', ~22% down): the Docker one-liner. Named '## Getting started' section is at line 164 (56% down) and contains NO commands — only a prose sentence plus links to the install page and per-SDK quick starts. The llms.txt itself never shows the curl installer; that lives in llms-full.txt line 580.
- Tone: Neutral-descriptive with a marketing frame ('the context layer for AI agents', pricing tiers, SOC 2 badges, customer stories). Makes NO demands of the agent — zero imperatives directed at the reader beyond 'Reach for SurrealDB when...' and the SSH 'Run ...' instructions. It is a router/index, not a protocol.
- Section order: # SurrealDB (front-matter: Last updated 2026-08-27; 'Markdown for agents' content-negotiation note; one-paragraph positioning) → ## When to use SurrealDB → ### How to call SurrealDB → ## Platform → ## SurrealDB → ## Studio → ## SurrealQL → ## Documentation → ## Products → ## AI and agent tools → ## Getting started → ## SDKs → ## Use cases → ## Customer stories → ## Industry solutions → ## SurrealDB Cloud → ## Resources → ## Why SurrealDB → ## Community → ## Company

### Install commands found

```sh
docker run --rm -p 8000:8000 surrealdb/surrealdb:latest start  (llms.txt line 65)
ssh surrealdb.sh  (llms.txt line 159)
ssh surrealdb.sh setup | claude  (llms.txt line 159)
ssh surrealdb.sh agents >> AGENTS.md  (llms.txt line 159)
```

### Text written for an agent (verbatim)

```text
No pasteable prompt exists in llms.txt or llms-full.txt. The closest agent-directed text is the line-159 entry: '- [Docs over SSH](https://surrealdb.sh): The full SurrealDB documentation as a read-only filesystem an agent can browse from a terminal. Run "ssh surrealdb.sh" and explore the docs at /surrealdb/docs with grep, cat, find and awk. No API key, no auth, isolated sandbox. Run "ssh surrealdb.sh setup | claude" to configure Claude Code with the doc context, or "ssh surrealdb.sh agents >> AGENTS.md" to append doc-checking rules to CLAUDE.md, GEMINI.md, AGENTS.md or any other agent rules file.'  The docs page https://surrealdb.com/docs/agents (200) SAYS 'Copy the setup prompt and paste it into your agent, or pick your agent below and follow the steps by hand.' followed by a client-rendered <AgentPrompt /> component whose text is NOT present in the .md twin nor in the preloaded JS chunks I fetched — I could not extract it verbatim; do not quote it.
```

### MCP snippet (verbatim)

```text
In llms.txt (lines 44-47): '- **MCP.** `https://mcp.surrealdb.com`, streamable HTTP. Deploy and manage\n  instances, run SurrealQL, and read and write agent memory as MCP tools.\n  Discovery: [server card](https://surrealdb.com/.well-known/mcp/server-card.json).\n  Authentication: OAuth 2.0 or a personal access token, described in\n  [auth.md](https://surrealdb.com/auth.md).'  No JSON config in either llms file. The JSON is on https://surrealdb.com/docs/build/ai-agents/mcp.md: '{\n  "mcpServers": {\n    "surrealdb": {\n      "url": "https://mcp.surrealdb.com"\n    }\n  }\n}' plus the token variant with '"headers": { "Authorization": "Bearer <your-token>" }'.
```

### Feedback / contribution ask (verbatim)

```text
None. The only community text is line 276: '- [GitHub](https://github.com/surrealdb/surrealdb): Source code and issue tracker' and line 277 '- [Discord](https://discord.gg/surrealdb): Community chat and support'. llms-full.txt line 820: '- **GitHub**: https://github.com/surrealdb/surrealdb - Source code, issues, and discussions.' No ask for reports, PRs, or feedback anywhere in either file.
```

### Best passage for driving install and first use (verbatim)

```text
### How to call SurrealDB

- **MCP.** `https://mcp.surrealdb.com`, streamable HTTP. Deploy and manage
  instances, run SurrealQL, and read and write agent memory as MCP tools.
  Discovery: [server card](https://surrealdb.com/.well-known/mcp/server-card.json).
  Authentication: OAuth 2.0 or a personal access token, described in
  [auth.md](https://surrealdb.com/auth.md).
- **SurrealQL over HTTP or WebSocket.** Query an instance directly. See the
  [REST API reference](https://surrealdb.com/docs/reference/rest-api).
- **SDKs.** Official clients for Rust, JavaScript, Python, Go, Java, .NET, PHP,
  and more. See the [SDK overview](https://surrealdb.com/docs/languages/overview).
- **CLI.** `surreal` starts, imports, exports, and queries a database from a
  shell. See the [CLI reference](https://surrealdb.com/docs/reference/cli).
- **Agent skills.** Eight installable skills covering SurrealQL, the CLI, the
  JavaScript and Python SDKs, vector search, and performance, indexed at
  [/.well-known/agent-skills/index.json](https://surrealdb.com/.well-known/agent-skills/index.json).
- **This website.** Every page has a markdown twin, and
  [openapi.json](https://surrealdb.com/openapi.json) describes the JSON this
  origin serves, including the
  [pricing catalogue](https://surrealdb.com/api/cloud/pricing.json).

To start an instance without an account, use the free tier at
[surrealdb.com/cloud](https://surrealdb.com/cloud), or run
`docker run --rm -p 8000:8000 surrealdb/surrealdb:latest start` locally.
```

### One-click links

- `None in llms.txt or llms-full.txt (no cursor://, vscode:, or npx ... add). The per-agent docs pages carry them: 'claude mcp add --transport http surrealdb https://mcp.surrealdb.com' (https://surrealdb.com/docs/agents/claude-code.md) and 'npx skills add surrealdb/agent-skills' (same page and https://surrealdb.com/docs/agents.md). Cursor page uses a mcp.json snippet, no cursor:// deep link.`

## llms-full.txt

- URL: <https://surrealdb.com/llms-full.txt> — HTTP 200
- Size: 51217 bytes
- Relationship to llms.txt: Curated hand-written brochure, NOT a concatenation of docs and NOT a superset of llms.txt. Same front-matter (positioning paragraph, 'Markdown for agents' note) but a different 'Last updated' (2026-08-18 vs 2026-08-27 for llms.txt), so they are maintained separately. Adds: a bulleted 'What is SurrealDB' fact list, 'Key features' by topic, agent-surface prose sections (Connect your agent, MCP Server, Platform API, Agent authentication (auth.md), Docs over SSH, Agent Skills), 40 lines of SurrealQL examples, Pricing, Comparisons (vs MongoDB/PostgreSQL/Neo4j/Elasticsearch), and the only actual install commands. The real docs dump is a THIRD file, https://surrealdb.com/docs/llms-full.txt (200, 7,106,657 bytes, 195,782 lines), and the docs index is https://surrealdb.com/docs/llms.txt (200, 95,985 bytes, 1,045 lines). So the split is: marketing llms.txt (28KB) / marketing llms-full.txt (51KB) / docs index (96KB) / docs dump (7MB).
- Install position: '## Getting started' at line 578 of 842 (69% down), with '### Install SurrealDB' at 580 (curl | sh for macOS and Linux, 'iwr https://install.surrealdb.com -useb | iex' for Windows, Docker), '### Start the server' at 602 ('surreal start memory'), '### Connect and query' at 610 (CLI + JS SDK snippet). Install comes AFTER Products, SurrealQL examples, SDKs, Security, Pricing, Use cases, Customer stories.

## Other agent-facing files

### docs-site llms.txt (docs index) — HTTP 200

<https://surrealdb.com/docs/llms.txt>

Auto-generated-style index of every docs page (1,045 lines), with a preamble that tells the agent how to read the site and 'Working notes'. Its '## Get started' section lists per-agent setup pages (Claude Code, Codex, Cursor, GitHub Copilot, VS Code, Windsurf, Zed), each described identically as 'Hosted MCP server and Agent Skills setup for X'. Install pages are bare titles under Reference. No installation commands in the index itself.

```text
This index lists every documentation page. Section landing pages carry a short description; the rest are titles only, so that the whole site fits in one index rather than a curated part of it. A missing description means nothing about the page - fetch any entry with ".md" appended to read it, or "https://surrealdb.com/docs/llms-full.txt" for everything at once.
```

### CLAUDE.md (repo) — HTTP 200

<https://raw.githubusercontent.com/surrealdb/surrealdb/main/CLAUDE.md>

Contributor-facing, not user-facing: crate map, build/test/bench commands, code-quality rules, a 'Bug Investigation Protocol'. Nothing about installing or promoting the product. Notable: it makes every bug report produce a reproduction file regardless of outcome.

```text
**Never assume bug reports are correct.** Always:

1. Check existing language tests in `language-tests/tests/` for related functionality
2. Verify expected behavior against SurrealQL docs (https://surrealdb.com/docs)
3. Create minimal reproduction test
4. Consider if this is user error, SDK issue, or actual bug
5. Create `language-tests/tests/reproductions/ISSUE_NUMBER_summary.surql` regardless of outcome
```

### AGENTS.md (repo) — HTTP 404

<https://raw.githubusercontent.com/surrealdb/surrealdb/main/AGENTS.md>

Does not exist.

### llms-install.md — HTTP 404

<https://surrealdb.com/llms-install.md>

Does not exist (also 404 at /docs/llms-install.md).

### SKILL.md (repo root) — HTTP 404

<https://raw.githubusercontent.com/surrealdb/surrealdb/main/SKILL.md>

Does not exist at repo root; skills live in the separate surrealdb/agent-skills repo (below) and are indexed at https://surrealdb.com/.well-known/agent-skills/index.json (200, 6,562 bytes).

### Agent setup page (per-agent picker + setup prompt) — HTTP 200

<https://surrealdb.com/docs/agents.md>

The single onboarding hub: a copy-paste setup prompt (client-rendered, not in .md), an agent picker (Claude Code/Codex/Cursor/Copilot/VS Code/Windsurf/Zed), and a fallback for any other MCP client. Every per-agent page follows the same skeleton: Add the MCP server / Sign in / Install the Agent Skills / Check it worked / Try it / Remove it / Next steps.

```text
Copy the setup prompt and paste it into your agent, or pick your agent below and follow the steps by hand. For a deeper reference on every way SurrealDB fits into AI tooling, see [AI agents](/docs/build/ai-agents.md).

<AgentPrompt />

## Pick your agent

Select an agent for its setup steps. Every agent listed supports both Skills and MCP.

<AgentPicker />

Using something else? Any MCP client can reach the hosted server. Add `https://mcp.surrealdb.com` as a remote server in whatever form the client accepts, then install the skills with `npx skills add surrealdb/agent-skills`.
```

### Claude Code setup page — HTTP 200

<https://surrealdb.com/docs/agents/claude-code.md>

61-line install page: one command adds MCP, one adds skills, one verifies, one removes; includes a headless (no-browser) token path and a first 'Try it' prompt.

````text
## Add the MCP server

```bash
claude mcp add --transport http surrealdb https://mcp.surrealdb.com
```

Add `--scope project` to write the entry to `.mcp.json` in the repository instead of your global configuration. The file holds no credentials, so it is safe to commit; everyone who opens the project signs in as themselves.

## Sign in

Run `/mcp` inside Claude Code, choose **surrealdb**, and approve the connection in the browser window that opens. Until you sign in, only the sign-in tool works.

If you are running somewhere without a browser, create a [personal access token](/docs/build/ai-agents/mcp.md#signing-in) and pass it as a header instead:

```bash
claude mcp add --transport http surrealdb https://mcp.surrealdb.com \
  --header "Authorization: Bearer <your-token>"
```

## Install the Agent Skills

Run this in your project root:

```bash
npx skills add surrealdb/agent-skills
```

## Check it worked

```bash
claude mcp list
```

**surrealdb** should be listed as connected.

## Try it

> Show me the SurrealDB instances in my organisation, and tell me which of them are paused.
````

### MCP Server docs page — HTTP 200

<https://surrealdb.com/docs/build/ai-agents/mcp.md>

Hosted-MCP install page (171 lines): the generic JSON config, client-specific variations, sign-in (OAuth vs token) with a permission table, tool groups, 'What the server will not do' safety list, and a Troubleshooting table of symptom -> action.

````text
For anything else, add `https://mcp.surrealdb.com` as a remote MCP server. The usual shape is:

```json
{
  "mcpServers": {
    "surrealdb": {
      "url": "https://mcp.surrealdb.com"
    }
  }
}
```

Some clients differ: VS Code uses a `servers` object with `"type": "http"`, and Windsurf and Antigravity use `serverUrl` in place of `url`. Once the server is connected, ask your assistant to list your organisations. If it comes back with them, you are set up.
````

### MCP server card — HTTP 200

<https://surrealdb.com/.well-known/mcp/server-card.json>

Machine-readable discovery document (728 bytes) with serverInfo, transport endpoint, capabilities and a documentation URL.

```text
"transport": {
        "type": "streamable-http",
        "endpoint": "https://mcp.surrealdb.com"
    },
    "capabilities": {
        "tools": {}
    },
    "documentation": "https://surrealdb.com/docs/build/ai-agents/mcp"
```

### auth.md — HTTP 200

<https://surrealdb.com/auth.md>

247-line 'auth.md format' document: which surface needs which credential and how an agent obtains one (OAuth 2.1 + DCR + PKCE, or a human-minted PAT). Explicitly states what is NOT implemented.

```text
Agent-attested registration is not implemented: there is no `/agent/identity` endpoint, no ID-JAG identity assertion intake, no `service_auth` provisioning, no anonymous identity and no claim ceremony. An account is always created by a human, and every credential derives from one.
```

### Docs over SSH (surrealdb.sh) — `agents` and `setup` outputs — HTTP 200

<ssh surrealdb.sh agents / ssh surrealdb.sh setup>

Live SSH sandbox serving the docs as a filesystem. `ssh surrealdb.sh agents` prints a markdown block meant to be appended to AGENTS.md/CLAUDE.md; `ssh surrealdb.sh setup` prints three options including piping into `claude` so the agent self-configures. (Fetched live over ssh, exit 0.)

````text
# SurrealDB Documentation — Agent Access

> Browse SurrealDB docs directly in your terminal via SSH.

## Quick start

```bash
ssh surrealdb.sh grep -rl 'SELECT' /surrealdb/docs
ssh surrealdb.sh cat /surrealdb/docs/surrealql/statements/select.mdx
ssh surrealdb.sh find /surrealdb/docs -name '*.mdx' | head -20
```

## Tips for agents

1. Start with `find /surrealdb/docs -name '*.mdx'` to discover available pages
2. Use `grep -rl '<keyword>' /surrealdb/docs` to find relevant files
3. Use `head -50` to skim before reading full files
4. The docs mirror surrealdb.com/docs — same markdown source
````

### agent-skills README — HTTP 200

<https://raw.githubusercontent.com/surrealdb/agent-skills/main/README.md>

Skill catalogue in the agentskills.io format with one-command install for all or per-skill, and a 'Use when:' trigger list per skill.

````text
### Install all skills

```bash
npx skills add surrealdb/agent-skills
```

### Install a specific skill

```bash
npx skills add surrealdb/agent-skills --skill surrealql
npx skills add surrealdb/agent-skills --skill surrealql-performance
```
````

### Agent guide (AGENTS.md) for Agent Memory — HTTP 200

<https://surrealdb.com/docs/agent-memory/reference/agents.md>

An imperative, agent-tone rules file published as a docs page, designed to be copied into .cursor/rules or a SKILL.md. Explicitly announces its tone and audience.

```text
This page is written for **coding agents** (Cursor, Claude Code, Copilot, and similar) building on SurrealDB Agent Memory. Humans can read it too, but the tone is imperative: what to do, what not to do, and where the sharp edges are.

> [!NOTE]
**Use it as a Cursor skill:** copy this file into `.cursor/rules/spectron.mdc`, add it as a project rule, or save it under `.cursor/skills/spectron/SKILL.md` with a short `description` in the frontmatter so the agent loads it when working on SurrealDB Agent Memory integrations.
```

## What they do better than XERJ

- Shell-less path is first-class: '### How to call SurrealDB' (llms.txt line 42) leads with MCP (hosted URL, streamable HTTP, a .well-known server card, auth.md), then HTTP/WS, then SDKs, and only then CLI. An agent with no terminal has a working route in the first screen. XERJ's file assumes a shell for steps 1-4 of 'First run' and relegates the HTTP-only path to a link ('Agent quickstart ... for an agent with no shell').
- Machine discoverability layer: /.well-known/mcp/server-card.json, /.well-known/agent-skills/index.json, /.well-known/oauth-protected-resource, openapi.json, and 'Accept: text/markdown' content negotiation on every page with x-markdown-tokens estimates. XERJ has tool schemas but no .well-known discovery or token-count headers.
- One-command MCP + one-command skills, per agent, with verify and uninstall: 'claude mcp add --transport http surrealdb https://mcp.surrealdb.com' / 'npx skills add surrealdb/agent-skills' / 'claude mcp list' / 'claude mcp remove surrealdb', each page ending in a 'Try it' prompt. Seven identical per-agent pages (Claude Code, Codex, Cursor, Copilot, VS Code, Windsurf, Zed). XERJ has 'xerj init' but no per-client pages and no verify/remove steps.
- 'Docs over SSH' with self-configuration: 'ssh surrealdb.sh setup | claude' and 'ssh surrealdb.sh agents >> AGENTS.md' are genuinely novel, zero-auth ways for an agent to ingest doc-checking rules without a browser or package manager.
- Positioning paragraph is followed immediately by a 'When to use' AND an explicit 'outside what it is built for' sentence (llms.txt lines 37-41), which scopes the tool before any install step. XERJ's Caveats section is analogous but comes after Start here.
- Curated llms-full.txt (51KB) that adds runnable examples and comparisons rather than repeating the index, with the 7MB docs dump kept as a separate third file so the agent chooses the depth.
- Headless credential path is documented next to the interactive one everywhere (PAT header on the Claude Code page; the MCP page's permission table and 'Include read:cloud alongside any of the others'). The 'What the server will not do' list and the symptom->action Troubleshooting table are the kind of pre-emptive failure-mode text XERJ's study showed agents need.
- The docs index preamble tells the agent how to read the index itself ('A missing description means nothing about the page - fetch any entry with ".md" appended') — it pre-empts the misreading XERJ saw with 'lexical read as neural'.

## What XERJ does better

- Honesty and caveat density: XERJ names its own sharp edges in the install text (PATH consequence, data-dir-inside-indexed-tree, fail-closed checksum, exit-code meanings, 'never invent a number' for progress). SurrealDB's llms.txt has no caveats about the product at all; its only 'not implemented' admission is in auth.md.
- Actual install commands in llms.txt: XERJ puts 'curl -fsSL https://xerj.org/get | sh' at line 11. SurrealDB's llms.txt has no installer command (only Docker at line 65); the curl installer is 580 lines into llms-full.txt.
- A contribution/feedback mechanism exists at all: 'xerj feedback --open-pr ...' with a CLA-exempt one-file PR, a dry-run, and an operator hand-off for sandboxed agents. SurrealDB asks nothing of the agent and offers only 'Source code and issue tracker'.
- Measured claims with numbers and a reproducibility promise ('Everything on this page is reproducible against a running XERJ node'). SurrealDB's numbers are prices and certifications, not benchmarks.
- Verbatim protocol for running long jobs for a human (estimate-first, relay progress, --max-minutes/--approve). No equivalent in SurrealDB's files.
- Explicit shape of the agent-facing API in the llms.txt (mcp-tools.json / anthropic-tools.json listing the ten tool names) rather than 'ask your assistant which SurrealDB tools it can use'.

## Adoptable ideas

- Add a '### How to call XERJ' block directly under the positioning paragraph, ordered MCP -> HTTP -> CLI, modelled on https://surrealdb.com/llms.txt lines 42-66: '- **MCP.** `https://mcp.surrealdb.com`, streamable HTTP. ... Discovery: [server card](https://surrealdb.com/.well-known/mcp/server-card.json).' For XERJ: '- **MCP.** `xerj mcp` (stdio, same binary) ... Discovery: https://xerj.org/.well-known/mcp/server-card.json' — this directly addresses the shell-less-persona failure and the unshipped-MCP finding.
- Publish a per-client setup page skeleton copied from https://surrealdb.com/docs/agents/claude-code.md: '## Add the MCP server' / '## Sign in' / '## Install the Agent Skills' / '## Check it worked' / '## Try it' / '## Remove it'. XERJ equivalent commands: `claude mcp add xerj -- xerj mcp`, `claude mcp list` ("**xerj** should be listed as connected."), and a '## Try it' line such as '> Find where BuiltinPager is defined and cite file:line.'
- Ship the sample MCP JSON verbatim in llms.txt, with the client variations sentence, from https://surrealdb.com/docs/build/ai-agents/mcp.md: '{ "mcpServers": { "surrealdb": { "url": "https://mcp.surrealdb.com" } } }' followed by 'Some clients differ: VS Code uses a `servers` object with `"type": "http"`, and Windsurf and Antigravity use `serverUrl` in place of `url`. Once the server is connected, ask your assistant to list your organisations. If it comes back with them, you are set up.' XERJ needs the stdio variant ('command': 'xerj', 'args': ['mcp']) plus a one-line 'you are set up when' test.
- Add a 'Copy the setup prompt and paste it into your agent' surface (https://surrealdb.com/docs/agents.md) — one pasteable paragraph that installs, starts a node, autoindexes and verifies. SurrealDB's prompt body is client-rendered and I could not extract it; XERJ should publish its own as plain text in llms.txt so it survives the .md twin.
- Publish the field-report rule as an appendable AGENTS.md block the way 'ssh surrealdb.sh agents >> AGENTS.md' does (https://surrealdb.sh, llms.txt line 159): e.g. `curl -fsSL https://xerj.org/agents.md >> AGENTS.md` containing the 'file one field report before the session ends' rule, so the obligation lives in the agent's persistent rules file instead of only in llms.txt. This is the structural fix for 'required' being filed under a heading named 'Optional': move the ask out of a link-list section into (a) step 5 of Start here (already there) and (b) a persistent rules snippet.
- Copy the 'Markdown for agents' front-matter convention (https://surrealdb.com/llms.txt line 5): 'every page on this site supports content negotiation. Request any URL with an "Accept: text/markdown" header ... served with "Content-Type: text/markdown" and "x-markdown-tokens" / "x-original-tokens" estimates ... append ".md" to any page path'. XERJ already serves .md answers; add the header + token estimates and say so in line 3-5.
- Add a 'What the server will not do' list and a symptom->action Troubleshooting table to the MCP/agent quickstart, modelled on https://surrealdb.com/docs/build/ai-agents/mcp.md ('| What you see | What to do |' ... 'Your assistant cannot find an organisation you named | It is looking for the organisation's identifier rather than its display name. Ask it to list your organisations first'). XERJ rows: 'command not found' -> PATH line; 'exit 3' -> success; 'connection refused on 9200' -> node not started/background-and-poll.
- Steal the docs-index preamble sentence that pre-empts misreading (https://surrealdb.com/docs/llms.txt line 7): 'A missing description means nothing about the page'. XERJ analogue for the 9-of-12 neural misread: put one bold sentence in the first 10 lines — 'Default search is lexical (feature-hashing); nothing on this page is neural unless `--embed-mode neural` is running.'
