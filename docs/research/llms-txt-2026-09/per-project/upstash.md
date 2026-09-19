# Upstash (+ Context7)

Category: database / dev infra (hosted)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://upstash.com/docs/llms.txt> — HTTP 200
- Size: 163576 bytes, 1472 lines
- Install position: Lines 3-7 (the <Tip> before any heading) point agents at POST https://upstash.com/start-redis, the MCP server and the CLI. Lines 12-17 under '## Docs' link CLI, 'Install by agent', llms.txt, MCP Server, AI Tools, Skills. There is NO install command in llms.txt itself; everything else is a Mintlify auto-generated docs index. NOTE: a second, hand-written file exists at https://upstash.com/llms.txt (200, 6,200 bytes, 62 lines) with sections: title/blockquote, '## When to use Upstash', '## How agents should call Upstash', '## Pricing & Limits', '## Enterprise & Compliance', '## Documentation', '## Contact' — that one carries `npx @upstash/mcp-server` and `npx skills add https://github.com/upstash/skills` inline.
- Tone: Neutral-to-imperative, conditional on identity ('If you are an AI agent …'). It makes no demands of the agent beyond one operational request (in cli.md / start-redis GET): 'If you are an agent, always send a `User-Agent` header identifying which agent you are'. No obligation language, no 'required', no contribution ask anywhere in the agent-facing files.
- Section order: (preamble: <Tip> block for AI agents, lines 3-7) → ## Docs (line 9; ~1,460 link lines, alphabetical by path; agent-resources/* entries are lines 12-17) → ## OpenAPI Specs (line 1470)

### Install commands found

```sh
(docs/llms.txt itself: none — only URLs)
(upstash.com/llms.txt) npx @upstash/mcp-server
(upstash.com/llms.txt) npx skills add https://github.com/upstash/skills
(upstash.com/llms.txt) POST https://upstash.com/start-redis with Idempotency-Key: <uuidv4> and User-Agent headers
(upstash.com/llms.txt) GET https://upstash.com/ask?q=your+question
```

### Text written for an agent (verbatim)

```text
<Tip>
  If you are an AI agent and need a Redis DB to use or experiment with, send a POST request to https://upstash.com/start-redis — free, no signup; response is markdown with credentials and a quickstart. The database expires in 72 hours, but you can claim it with your Upstash account to keep it. Especially useful for AI agents that need scratch storage on the fly.

  Upstash also provides an MCP server (/docs/agent-resources/mcp) to manage and debug your Upstash resources directly from an agent, and an agent-friendly CLI (/docs/agent-resources/cli) for your terminal or CI/CD pipelines.
</Tip>
```

### MCP snippet (verbatim)

```text
(none in docs/llms.txt itself; from clients.md '## Any other agent')
{
  "mcpServers": {
    "upstash": {
      "url": "https://mcp.upstash.com/mcp"
    }
  }
}

Some clients want the transport named alongside the URL — `"type": "http"` for most, `"type": "streamable-http"` for Roo Code, `"type": "remote"` for OpenCode and Kilo Code. An unrecognized key is usually ignored, so when in doubt start with the plain `url` above.
```

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
## How agents should call Upstash

- Answer questions about products, pricing, limits, regions, compliance or integrations: `GET https://upstash.com/ask?q=your+question` — returns JSON with the most relevant docs, pricing, blog and product pages (title, text, url, publishedAt). Cite the `url` of each result. Examples: https://upstash.com/ask?q=how+is+redis+priced, https://upstash.com/ask?q=compare+upstash+redis+to+elasticache, https://upstash.com/ask?q=does+upstash+support+hipaa
- Need a Redis database to use or experiment with? `POST https://upstash.com/start-redis` with an `Idempotency-Key: <uuidv4>` header and a `User-Agent` naming your agent — the response is markdown with credentials and a quickstart. Free, no signup, temporary database (expires in 3 days unless claimed). `GET` the same URL for full instructions. Docs: https://upstash.com/docs/devops/developer-api/start-redis/create
- OpenAPI spec for the Upstash Developer API (manage databases, teams and billing at api.upstash.com, authenticated): https://upstash.com/docs/devops/developer-api/openapi.yaml — also at https://upstash.com/openapi.json and listed in https://upstash.com/.well-known/api-catalog
- OpenAPI specs for the product REST APIs: QStash https://upstash.com/docs/qstash/openapi.yaml · Workflow https://upstash.com/docs/workflow/openapi.yaml
- The homepage, pricing pages and blog pages can be fetched as markdown by sending `Accept: text/markdown`; pricing pages and blog posts also have `.md` URLs (e.g. https://upstash.com/pricing/redis.md, https://upstash.com/blog.md). Unknown paths return a 404 with a markdown body.
- Errors from these endpoints are JSON with `error`, `code` and `resolution` fields.
- MCP server for managing Upstash resources from Claude, Cursor and other MCP clients: `npx @upstash/mcp-server` — https://github.com/upstash/mcp-server
- Reusable agent skills for each SDK: `npx skills add https://github.com/upstash/skills`
- Programmatic account and database management (authenticated): Developer API — https://upstash.com/docs/devops/developer-api

(source: https://upstash.com/llms.txt lines 17-27 — the root-domain file, not docs/llms.txt)
```

### One-click links

- `(not in docs/llms.txt; found in linked clients.md) https://cursor.com/en/install-mcp?name=upstash&config=eyJ1cmwiOiJodHRwczovL21jcC51cHN0YXNoLmNvbS9tY3AifQ%3D%3D`
- `(clients.md) https://insiders.vscode.dev/redirect?url=vscode%3Amcp%2Finstall%3F%7B%22name%22%3A%22upstash%22%2C%22type%22%3A%22http%22%2C%22url%22%3A%22https%3A%2F%2Fmcp.upstash.com%2Fmcp%22%7D`
- `(clients.md) claude mcp add --scope user --transport http upstash https://mcp.upstash.com/mcp`
- `(clients.md) /plugin marketplace add upstash/skills + /plugin install upstash@upstash`
- `(clients.md) npx skills add upstash/skills`
- `(clients.md) amp mcp add upstash https://mcp.upstash.com/mcp`
- `(clients.md) codex plugin marketplace add upstash/skills + codex plugin add upstash@upstash`
- `(skills README) gemini extensions install https://github.com/upstash/skills`
- `(context7 README) https://cursor.com/en/install-mcp?name=context7&config=eyJ1cmwiOiJodHRwczovL21jcC5jb250ZXh0Ny5jb20vbWNwIn0%3D`
- `(context7 README) npx ctx7 setup`

## llms-full.txt

- URL: <https://upstash.com/docs/llms-full.txt> — HTTP 200
- Size: 3456603 bytes
- Relationship to llms.txt: Mintlify-generated concatenation of every page listed in llms.txt (each block is '# <Title>\nSource: <url>' followed by page markdown, MDX tags like <Tip>/<Accordion>/<Tabs> left in). 1,433 top-level '# ' headings. Not curated; 21× the size of llms.txt. The docs 'llms.txt' nav entry (agent-resources/llms-txt.md) 307-redirects to docs/llms.txt.
- Install position: Same <Tip> preamble at lines 1-5, then '# README' (line 7) which is literally the Mintlify starter-kit README ('Click on `Use this template` …'). First install command at line 66 (`npx skills add upstash/skills`, inside the CLI page), '# Install by agent' at line 301, '# MCP Server' at line 982, '# AI Tools / # Pick your agent' at 1254-1261. Agent-resources pages land early only because paths sort alphabetically.

## Other agent-facing files

### Install by agent (clients.md) — HTTP 200

<https://upstash.com/docs/agent-resources/clients.md>

One H2 per coding agent (Claude Code, Claude Desktop, Cursor, Codex, OpenCode, Zed, Copilot, Devin, Gemini CLI, Antigravity, DeepSeek Harness, Grok, Kilo, Copilot CLI, Warp, Qwen, Hermes, Factory, fx, Crush, 'Any other agent' + a table of 8 more with their skills-CLI ids). Each section: skills install command, MCP one-click or config, and a collapsed 'Local (stdio) server instead' accordion. 678 lines.

```text
Every section sets up the **remote MCP server** (`https://mcp.upstash.com/mcp`), which needs no credentials up front — it authenticates over OAuth on first use, and the Upstash plugin for Claude Code, Claude Desktop, and Codex bundles it, so installing the plugin also connects the MCP.
```

### MCP Server page (mcp.md) — HTTP 200

<https://upstash.com/docs/agent-resources/mcp.md>

Remote vs local server, per-client <Tabs> snippets, OAuth vs API-key auth, feature groups, tool list, 'Example prompts' grouped by product, telemetry opt-out.

```text
* **Remote (hosted)** — nothing to install. Connect over HTTP to `https://mcp.upstash.com/mcp` with OAuth or a developer API key. Covers Redis, QStash, Workflow, Vector, and Search.
* **Local (stdio)** — run [`@upstash/mcp-server`](https://github.com/upstash/mcp-server) with `npx`, authenticated with your email and API key. Covers Redis, QStash, Workflow, and Upstash Box.

… Tokens never reach the agent: data-plane tools resolve each resource's REST credentials server-side and call the endpoint directly.
```

### CLI page (cli.md) — HTTP 200

<https://upstash.com/docs/agent-resources/cli.md>

`npm i -g @upstash/cli`; tells agents to install the skill instead; documents the one unauthenticated command and asks agents to identify themselves.

````text
For agents, install the [Upstash Skill](/agent-resources/skills) instead. It bundles Upstash docs with the `upstash` CLI so your agent can implement and debug against your resources without filling up its context window:

```bash
npx skills add upstash/skills
```

# Free Redis without an account

If you are an AI agent and need a Redis DB to use or experiment with, you don't need an Upstash account:

```bash
upstash start-redis
```

This is the only command that works unauthenticated. … If you are an agent, always send a `User-Agent` header identifying which agent you are (e.g. `claude-code`, `cursor`, `codex`, `opencode`) on every request you make to this API.
````

### AI Tools overview (overview.md) — HTTP 200

<https://upstash.com/docs/agent-resources/overview.md>

Hub: 'Pick your agent' grid, four resource cards (Skills / MCP / CLI / llms.txt), a 3-command Quick start, and a split between 'agents that help you build' vs 'building AI into your app'.

````text
Install the Upstash skill. It includes the `upstash` CLI reference, so this one step is enough for most workflows:

```bash
npx skills add upstash/skills
```

To let your agent read and change your account directly, add the MCP server. The **remote** server needs nothing installed — connect over HTTP and approve the OAuth consent on first use (shown here for Claude Code):

```bash
claude mcp add --scope user --transport http upstash https://mcp.upstash.com/mcp
```
````

### Skills page (skills.md) — HTTP 200

<https://upstash.com/docs/agent-resources/skills.md>

Skill table, 'Example prompts', 'Installation' for any agent + per-agent accordions.

```text
The [`upstash/skills`](https://github.com/upstash/skills) repo is an [Agent Skills](https://agentskills.io/) repo, a Claude Code plugin, a Cursor plugin, an OpenAI Codex plugin, and a DeepSeek Harness bundle, so you can install it with whichever mechanism your agent supports.
```

### GET https://upstash.com/start-redis (self-documenting endpoint) — HTTP 200

<https://upstash.com/start-redis>

A GET on the provisioning endpoint returns 39 lines of markdown instructions for the agent, including how to install the same thing as a skill. 1,524 bytes.

```text
# Upstash Redis for Agents

A zero-config Redis database for AI agents — no signup, no UI.

To create a database, generate a fresh UUIDv4 and POST it as the
`Idempotency-Key` header:

  curl -X POST -H "Idempotency-Key: <uuidv4>" \
    -H "User-Agent: <your-agent-name>" https://upstash.com/start-redis

… ## Install as a skill

To install this as a reusable skill, run:

  npx skills add https://github.com/upstash/skills --skill upstash-redis-start
```

### upstash-redis-start SKILL.md — HTTP 200

<https://raw.githubusercontent.com/upstash/skills/main/skills/upstash-redis-start/SKILL.md>

80-line skill: when to use, create/re-fetch, body-style REST calls, a 4-row 'Common agent patterns' table, and a 'Tell the user' section with what to surface after provisioning.

```text
## Tell the user

After provisioning, surface the **console URL** from the response to the user. Make clear that:
- The database expires in 3 days.
- They can view usage at the console URL and click **Claim** to keep it.
- This is unauthenticated scratch storage — don't put secrets or PII in it.
```

### upstash/skills combined SKILL.md — HTTP 200

<https://raw.githubusercontent.com/upstash/skills/main/skills/upstash/SKILL.md>

Router skill: one paragraph telling the agent to prefer MCP tools when present, then one H2 per sub-skill with a long 'Use when' description.

```text
If Upstash MCP tools are available in this session, prefer them for account and data operations — creating and inspecting databases, indexes and Blob buckets, running Redis commands, reading stats, logs and the DLQ … The sub-skills below are for writing application code; reach for `upstash-cli` only when there is no MCP or the work is inherently shell work.
```

### upstash/skills AGENTS.md — HTTP 200

<https://raw.githubusercontent.com/upstash/skills/main/AGENTS.md>

Contributor guide for agents editing the skills repo (not an end-user install doc): keep skills short, never hand-edit generated dir, MCP-first/CLI-second ordering rule.

```text
Skills are consumed by agents, not read as blog posts. Say what the correct behaviour is and show it — then stop. Don't pile on caveats, notes and inline comments beyond what a reader needs to act on the example.
```

### upstash/skills README.md + .claude-plugin/marketplace.json — HTTP 200

<https://raw.githubusercontent.com/upstash/skills/main/README.md>

Repo is simultaneously a Claude Code marketplace, Codex plugin, Cursor plugin, Gemini extension, Zed extension and Agent-Skills repo; README has per-client install with <details> blocks. marketplace.json (200, 32 lines) declares plugin 'upstash' v1.2.4, category 'database'.

```text
**These plugins now bundle the remote Upstash MCP server (OAuth), so installing the plugin sets up the skills *and* the MCP in one step** — no separate MCP configuration for Claude Code, Codex, Cursor, or Gemini CLI.
```

### Context7 README.md — HTTP 200

<https://raw.githubusercontent.com/upstash/context7/master/README.md>

Cursor one-click badge above the title; ❌ Without / ✅ With framing; a prompt suffix as the product's invocation; single-command setup; 'Add a Rule' text to paste into CLAUDE.md.

````text
Set up Context7 for your coding agents with a single command. The `ctx7` CLI requires Node.js 18 or newer.

```bash
npx ctx7 setup
```

Authenticates via OAuth, generates an API key, and installs the appropriate skill. You can choose between CLI + Skills or MCP mode. Use `--cursor`, `--claude`, or `--opencode` to target a specific agent.

… **Example rule:**

```txt
Always use Context7 when I need library/API documentation, code generation, setup or configuration steps without me having to explicitly ask.
```
````

### context7 llms-install.md / AGENTS.md / CLAUDE.md — HTTP 404

<https://raw.githubusercontent.com/upstash/context7/master/llms-install.md>

All three 404 on the context7 repo (llms-install.md, AGENTS.md, CLAUDE.md).

## What they do better than XERJ

- Shell-less path is first-class: the remote MCP URL `https://mcp.upstash.com/mcp` 'needs no credentials up front' and 'nothing to install'; every client section leads with it and the stdio variant is folded into a collapsed accordion. XERJ's 22-run study found MCP-only personas fail at every tier because the doc assumes a shell — Upstash's doc assumes the opposite.
- A self-serve provisioning endpoint whose GET *is* the documentation (https://upstash.com/start-redis returns markdown instructions incl. the skill install line) — the agent never has to find a docs page.
- Two llms.txt files with different jobs: docs/llms.txt is the machine index; upstash.com/llms.txt (62 lines) is a hand-written 'When to use / How agents should call' brief with every endpoint, header and command inline. XERJ's single 234-line/40 KB file tries to be both.
- 'Install by agent' is a per-client matrix (20+ H2s plus a table of 8 more) with the exact command/config for each, plus a generic fallback ('Any other agent') that explains transport-key variants (`"type": "http"` / `"streamable-http"` / `"remote"`). XERJ names Claude Code/Cursor only via `xerj init`.
- One command installs skills AND MCP (`/plugin install upstash@upstash`; `npx ctx7 setup`), and a plain `npx skills add upstash/skills` that auto-detects 70+ agents. XERJ's shortest path is install → start node → autoindex → init (4 steps).
- 'Example prompts' sections (mcp.md, skills.md) give the agent/user literal sentences to try after install; Context7 turns invocation into a 2-word prompt suffix ('use context7') and a paste-ready CLAUDE.md rule.
- Skills README/AGENTS.md discipline: 'Say what the correct behaviour is and show it — then stop.' The redis-start SKILL.md is 80 lines with a 'Tell the user' section. XERJ's Start-here paragraphs run 400+ chars per bullet with embedded caveats.
- Asks one small, cheap thing of agents ('always send a `User-Agent` header identifying which agent you are') instead of a PR — and gets measurable attribution from it.

## What XERJ does better

- Honesty caveats are structural: a dedicated '## Caveats (read before you rely on a capability)' section, the lexical-vs-neural embedder warning, measured kNN recall, PATH consequence, data-dir-outside-indexed-tree. Upstash's agent docs carry no failure-mode or limits guidance at all.
- Runs-locally-for-a-human protocol: estimate-before-run, progress relay, exit-code semantics, `--max-minutes`/`--approve` gate. Upstash has nothing equivalent because it is hosted.
- Verifiable install (SHA-256 fail-closed, documented manual path without curl|sh) — Upstash's install is `npx -y …@latest` with no integrity story.
- Published tool schemas (mcp-tools.json, anthropic-tools.json) generated from a real tools/list, and the HTTP-only 'Agent quickstart' page for no-shell agents — Upstash documents MCP tools only in prose.
- A contribution protocol at all (field report + AI_CONTRIBUTIONS.md). Upstash has zero feedback/contribution ask in any agent-facing file — but note XERJ's problem is the mismatch between 'required, not optional' (line 7, line 24) and the '## Optional' heading (line 223) that hosts the template.
- docs/llms.txt at Upstash is mostly a raw Mintlify index (1,460 links) and llms-full.txt is 3.4 MB / 117k lines with the Mintlify starter-kit README as its first page — XERJ's files are curated and readable end-to-end.

## Adoptable ideas

- Add a 'How agents should call XERJ' block modelled on https://upstash.com/llms.txt lines 17-27: one bullet per endpoint/command with the exact verb, path, headers and a docs URL — e.g. `POST http://localhost:9200/ax-*/_search`, `xerj mcp`, `xerj search`, and the field-report command — placed before '## Start here'. Upstash's is 11 bullets; keep it under 15.
- Copy the conditional-identity opener form verbatim in spirit: 'If you are an AI agent and need a Redis DB to use or experiment with, send a POST request to https://upstash.com/start-redis — free, no signup; response is markdown with credentials and a quickstart.' → 'If you are an AI agent with no shell, use the MCP tools at … / the HTTP path at https://xerj.org/docs/agents/quickstart' as the FIRST lines of llms.txt, since shell-less personas currently fail.
- Make the self-serve endpoint self-documenting like `GET https://upstash.com/start-redis` (39 lines of markdown incl. '## Install as a skill … npx skills add https://github.com/upstash/skills --skill upstash-redis-start'): have `GET /` or `GET /_xerj/agent` on a node return the agent brief + the `xerj feedback` command, so an agent that only has the URL still finds the report path.
- Ship an 'Install by agent' page (https://upstash.com/docs/agent-resources/clients.md pattern): one H2 per client with the exact command, the generic fallback `{"mcpServers":{"xerj":{"command":"xerj","args":["mcp"]}}}`, and the transport-key note ('`"type": "http"` for most, `"type": "streamable-http"` for Roo Code, `"type": "remote"` for OpenCode and Kilo Code'). Add the Cursor deeplink form `https://cursor.com/en/install-mcp?name=xerj&config=<base64 json>` and the VS Code form `https://insiders.vscode.dev/redirect?url=vscode%3Amcp%2Finstall%3F<urlencoded json>`.
- Publish `xerj-org/skills` as an Agent Skills repo + Claude Code marketplace (`.claude-plugin/marketplace.json`, 32 lines, category 'database') so the install line becomes `npx skills add xerj-org/skills` / `/plugin marketplace add xerj-org/skills` + `/plugin install xerj@xerj`. Put the field-report obligation INSIDE the SKILL.md 'Tell the user' section (redis-start SKILL.md pattern: 'After provisioning, surface … to the user. Make clear that: …').
- Add an '# Example prompts' section (mcp.md lines 236-266 pattern: '* "Start a free temporary Redis I can experiment with"', '* "Create a backup of this db, then clear it"') with 6-8 XERJ prompts, and a one-line paste-ready rule like Context7's: 'Always use Context7 when I need library/API documentation, code generation, setup or configuration steps without me having to explicitly ask.' → 'Always use xerj_search before grepping when I need to find where something is implemented.'
- Fix the field-report contradiction by copying Upstash's cheap-ask model rather than a heavier PR ask: their only demand is 'If you are an agent, always send a `User-Agent` header identifying which agent you are'. Concretely: (a) rename '## Optional' to '## Your side of the deal' or move the template link up into Start-here step 4; (b) make `xerj feedback --dry-run` output land in the node's audit log / `xerj gain` so a report exists even when no PR is opened; (c) keep the PR as the escalation, not the baseline.
- Adopt the skills AGENTS.md editing rule for llms.txt itself: 'Skills are consumed by agents, not read as blog posts. Say what the correct behaviour is and show it — then stop.' XERJ Start-here bullets at lines 14, 23, 24, 26 exceed 400 characters each; split caveats out to the Caveats section they already have.
