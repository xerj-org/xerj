# Firecrawl

Category: agent tooling (web data API) / MCP server

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.firecrawl.dev/llms.txt> — HTTP 200
- Size: 8815 bytes, 128 lines
- Install position: absent as text. The only install-adjacent content is one link line (line 12, under '#### Get Started'): "- [Get Started](https://docs.firecrawl.dev/mcp-server.md): Set up Firecrawl MCP with keyless access, account sign-in, or an API key." No command appears anywhere in llms.txt. It is a plain Mintlify auto-generated docs index (title + one-line description per page); installation lives one hop away in mcp-server.md / introduction.md.
- Tone: neutral / catalog. Zero imperatives toward the agent apart from the auto-generated recursion hint "Follow each `/_llms/` index recursively until you reach documentation pages." Makes no demands, asks for nothing back.
- Section order: # Firecrawl Docs (H1) → - v2 (201 pages) link line 3 → ## v1 → ### Documentation → #### Get Started (Introduction / Get Started=mcp-server.md / Advanced Scraping Guide) → ##### Plans and Billing → ##### Enterprise → #### Standard Features (Crawl, Map, Search) → ##### Scrape → #### Webhooks → #### Dashboard → ### SDKs / #### Overall / #### Official (Python..Elixir) → ## OpenAPI Specs (26 links, many locale duplicates) → ## Optional (Playground, Blog, Community, Changelog, Integrations) → > blockquote: 'The links below point to documentation indexes. Follow each `/_llms/` index recursively until you reach documentation pages.' → ## Indexes (14 locale/version sub-indexes)

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
#### Get Started

- [Introduction](https://docs.firecrawl.dev/introduction.md): The web data API for AI agents. Search the web, scrape any page, and interact with it through one API.
- [Get Started](https://docs.firecrawl.dev/mcp-server.md): Set up Firecrawl MCP with keyless access, account sign-in, or an API key.
- [Advanced Scraping Guide](https://docs.firecrawl.dev/advanced-scraping-guide.md): Configure scrape options, browser actions, crawl, map, and the agent endpoint with Firecrawl's full API surface.
```

## llms-full.txt

- URL: <https://docs.firecrawl.dev/llms-full.txt> — HTTP 200
- Size: 1914650 bytes
- Relationship to llms.txt: Auto-generated concatenation of every docs page (661 H1s), each block headed '# <Title>' + 'Source: <url>'. Not curated; alphabetical-ish by path, so the file OPENS with 'Advanced Scraping Guide' (line 1), then 'Build with AI' (line 669, the /ai-onboarding page), then Billing. Introduction (with the install block) appears at line 16513; the MCP Get Started page at 16975 and again at 44334 (localized duplicates). 1.9 MB — far beyond any single context window, so agents effectively must use llms.txt + per-page .md files instead.
- Install position: First install command is 'npx -y firecrawl-cli@latest init --all --browser' at line 721 (inside 'Build with AI', ~1.4% into the file). First 'claude mcp add' at line 16529 (Introduction page).

## Other agent-facing files

### MCP Get Started (mcp-server.md) — HTTP 200

<https://docs.firecrawl.dev/mcp-server.md>

Three-card auth chooser (Try Instantly / Sign in / Use an API key) + remote endpoint + secret-handling rule; separate 'For Agents' vs 'For Humans' client-setup cards. Every docs .md page opens with a 3-line blockquote pointing back to llms.txt.

```text
> ## Documentation Index
> Fetch the complete documentation index at: https://docs.firecrawl.dev/llms.txt
> Use this file to discover all available pages before exploring further.
[...]
Configure the key through an environment variable or your client's secret storage, never in the MCP URL.
[...]
  <Card title="For Agents" icon="robot" href="/mcp-server/keyless">
    Start keyless or use an API key.
  </Card>

  <Card title="For Humans" icon="user" href="/mcp-server/oauth">
    Sign in via browser.
  </Card>
```

### For Agents (mcp-server/keyless.md) — HTTP 200

<https://docs.firecrawl.dev/mcp-server/keyless.md>

Shell-less install path: one remote URL, per-client one-liners (Codex, Claude Code, Cursor JSON, OpenCode JSON), a 'Verify your connection' step naming the exact tool names to expect, and a copy-paste first prompt under '## Try'.

````text
Keyless server URL (Streamable HTTP, no credential): `https://mcp.firecrawl.dev/v2/mcp`

* Codex: `codex mcp add firecrawl --url https://mcp.firecrawl.dev/v2/mcp`
* Claude Code: `claude mcp add --transport http firecrawl https://mcp.firecrawl.dev/v2/mcp`
* Cursor or any JSON-config client: `{"mcpServers": {"firecrawl": {"url": "https://mcp.firecrawl.dev/v2/mcp"}}}`
* OpenCode (`opencode.json`): `{"mcp": {"firecrawl": {"type": "remote", "url": "https://mcp.firecrawl.dev/v2/mcp", "enabled": true}}}`
[...]
## Verify your connection

Open your client's MCP status or tool list and confirm that `firecrawl` is connected. A keyless connection shows `firecrawl_search`, `firecrawl_scrape`, and `firecrawl_parse`; [...]

## Try

```text theme={null}
Search the web for the latest Firecrawl release notes and summarize the sources.
```

If no Firecrawl tools appear, restart or reload the client after saving its MCP configuration.
````

### For Humans (mcp-server/oauth.md) — HTTP 200

<https://docs.firecrawl.dev/mcp-server/oauth.md>

OAuth endpoint variant of the same per-client list; explicit human-in-the-loop boundary.

```text
* Claude Code: `claude mcp add --transport http firecrawl https://mcp.firecrawl.dev/v2/mcp-oauth` then complete sign-in via `/mcp`
[...]
A human must complete the browser sign-in and approve a team. Do not open the server URL directly in a browser.
```

### Run locally (mcp-server/local.md) — HTTP 200

<https://docs.firecrawl.dev/mcp-server/local.md>

stdio / Streamable HTTP local server with pinned version, health check, env-var table, troubleshooting accordions (spawn npx ENOENT, etc.).

````text
env FIRECRAWL_API_KEY=fc-YOUR-API-KEY \
  npx -y firecrawl-mcp@3.23.7
[...]
Confirm the server is ready:

```bash theme={null}
curl http://localhost:3000/health
```

The health check returns `ok`. The local route is `/mcp`; `/v2/mcp` belongs to the hosted Firecrawl service.
````

### MCP tools (mcp-server/tools.md) — HTTP 200

<https://docs.firecrawl.dev/mcp-server/tools.md>

Job-to-tool routing table ('Read one page' -> firecrawl_scrape ...), tool availability by connection mode, and feedback tools listed as a first-class job.

```text
| Send product feedback         | `firecrawl_search_feedback` and `firecrawl_feedback` | You want to rate search results or report endpoint-level quality.                                                                                                                      |
```

### Introduction (introduction.md) — HTTP 200

<https://docs.firecrawl.dev/introduction.md>

Landing page: MCP one-liners first, then keyless curl, then CLI init, then a 'Setup for agents' copy button whose payload is a one-line prompt pointing at SKILL.md, then an explicit llms.txt/llms-full.txt note.

```text
export const AgentSetupButton = () => {
  const prompt = "Read and follow https://www.firecrawl.dev/agent-onboarding/SKILL.md";
[...]
### Build directly with the API

Scrape your first page now. No account or API key is required for this request.
[...]
### Set up with an agent

Provide your agent with this Firecrawl setup prompt, or see [all MCP setup options](/mcp-server).

<AgentSetupButton />

<Note>
  **For AI agents:** Use [llms.txt](https://docs.firecrawl.dev/llms.txt) for an index of the documentation, or [llms-full.txt](https://docs.firecrawl.dev/llms-full.txt) for the full text.
</Note>
```

### agent-onboarding/SKILL.md (the real agent install doc; NOT linked from llms.txt) — HTTP 200

<https://www.firecrawl.dev/agent-onboarding/SKILL.md>

16,282-byte skill-format file: Install -> verify -> Get Credentials -> 'How you might arrive' -> 'Choose Your Path' A-F (live tools / app code / deliverables / human auth / no-install REST / keyless fallback). Includes a PKCE-style flow where the agent generates a session, hands the human a URL, polls for the API key, and writes it to .env.

````text
## Install

The command below installs the Firecrawl CLI, the core CLI skills, and
the workflow skills. It also opens browser auth so the human can sign
in or create an account.

```bash
npx -y firecrawl-cli@latest init --all --browser
```
[...]
Before doing real work, verify the install:

```bash
mkdir -p .firecrawl
firecrawl --status
firecrawl scrape "https://firecrawl.dev" -o .firecrawl/install-check.md
```
[...]
**How you might arrive:**

- **Docs or website sent you here** — continue with Choose Your Path
  below for CLI/skills/MCP onboarding.
- **API `401`:** Path D is the default. [...]
- **Already have `FIRECRAWL_API_KEY`** — skip credential setup; pick
  Path A–E below.
[...]
- **Don't want to install anything** -> Path E (REST API directly)
- **No API key and the human cannot sign up right now** -> Path F (keyless free tier, fallback)
````

### AGENTS.md (firecrawl/firecrawl) — HTTP 200

<https://raw.githubusercontent.com/firecrawl/firecrawl/main/AGENTS.md>

Contributor-facing only (18 lines): monorepo map + the test-first workflow. Nothing about installing/using the product.

```text
1. Write some end-to-end tests that assert your win conditions, if they don't already exist
  - 1 happy path (more is encouraged if there are multiple happy paths with significantly different code paths taken)
  - 1+ failure path(s)
[...]
4. Push to a branch, open a PR, and let CI run to verify your win condition.
Keep these steps in mind while building your TODO list.
```

### CLAUDE.md (firecrawl/firecrawl) — HTTP 200

<https://raw.githubusercontent.com/firecrawl/firecrawl/main/CLAUDE.md>

Identical to AGENTS.md plus one extra paragraph about the knip pre-commit hook.

```text
Never bypass `knip` failures (e.g. with `git commit --no-verify`). If the pre-commit `knip` check fails, fix the reported unused exports/files — even if they predate your change — before committing.
```

### firecrawl-mcp-server README — HTTP 200

<https://raw.githubusercontent.com/firecrawl/firecrawl-mcp-server/main/README.md>

39 KB, 1023 lines: 'When to Use This Server' before Installation; hosted keyless first, then npx, Cursor, Windsurf, VS Code one-click badges; two feedback TOOLS wired into the protocol with a credit refund; 4-line Contributing.

```text
Never put an API key in the server URL. Never put an API key in an agent chat. Configure it directly in the client or secret manager. See the [hosted MCP setup guide](https://docs.firecrawl.dev/mcp-server) and the [agent onboarding guide](https://www.firecrawl.dev/agent-onboarding/SKILL.md) for client-specific instructions.
[...]
### 3b. Search Feedback Tool (`firecrawl_search_feedback`)

Sends structured feedback on a previous `firecrawl_search` result. The first feedback per search id refunds 1 credit and improves Firecrawl's search quality. Idempotent per search id.

**Call this after every search you actually use** (or that didn't help). Bad/partial feedback with `missingContent` is just as valuable as good feedback.
[...]
**Most important field:** `missingContent`. It's an array of specific pieces of content the agent expected to find but did not. One entry per missing topic — these aggregate across teams and tell us what to index next.
[...]
### Contributing

1. Fork the repository
2. Create your feature branch
3. Run tests: `npm test`
4. Submit a pull request
```

### VS Code one-click install badge (from README line 179) — HTTP 200

<https://insiders.vscode.dev/redirect/mcp/install?name=firecrawl&inputs=%5B%7B%22type%22%3A%22promptString%22%2C%22id%22%3A%22apiKey%22%2C%22description%22%3A%22Firecrawl%20API%20Key%22%2C%22password%22%3Atrue%7D%5D&config=%7B%22command%22%3A%22npx%22%2C%22args%22%3A%5B%22-y%22%2C%22firecrawl-mcp%22%5D%2C%22env%22%3A%7B%22FIRECRAWL_API_KEY%22%3A%22%24%7Binput%3AapiKey%7D%22%7D%7D>

Only one-click deeplink found; no cursor:// links in README or docs.

```text
[![Install with NPX in VS Code](https://img.shields.io/badge/VS_Code-NPM-0098FF?style=flat-square&logo=visualstudiocode&logoColor=white)](https://insiders.vscode.dev/redirect/mcp/install?name=firecrawl&inputs=...)
```

### llms-install.md — HTTP 404

<https://docs.firecrawl.dev/llms-install.md>

does not exist

### SKILL.md at repo root (firecrawl/firecrawl) — HTTP 404

<https://raw.githubusercontent.com/firecrawl/firecrawl/main/SKILL.md>

does not exist at repo root; the skill lives at www.firecrawl.dev/agent-onboarding/SKILL.md and in the firecrawl/cli, firecrawl/skills, firecrawl/firecrawl-workflows repos

## What they do better than XERJ

- Shell-less path is first-class and needs zero local process: a hosted Streamable-HTTP MCP endpoint (`https://mcp.firecrawl.dev/v2/mcp`) with per-client one-liners for Codex, Claude Code, Cursor JSON and OpenCode JSON. XERJ's MCP requires a running node plus `xerj mcp` stdio, which is exactly the persona the 22-run study says fails at every tier.
- Keyless / no-credential first use, explicitly labelled a fallback: "No account or key. Search, Scrape, and Parse within daily limits." Removes the sign-up gate from hop 1; upgrade path stated afterwards.
- A 'Verify your connection' step that names the exact tool names the agent should see (`firecrawl_search`, `firecrawl_scrape`, `firecrawl_parse`) and a single copy-paste '## Try' prompt, followed by one recovery line ('If no Firecrawl tools appear, restart or reload the client'). XERJ has a verify path for install but not a 'you should now see these MCP tools' check.
- A one-line agent prompt on the human landing page behind a copy button: "Read and follow https://www.firecrawl.dev/agent-onboarding/SKILL.md" — the human pastes 7 words, the agent fetches a skill-format doc with Paths A-F. XERJ's hero prompt is already found on hop 1, but XERJ has no equivalent skill-format URL an agent is told to 'read and follow'.
- 'How you might arrive' routing in SKILL.md (Docs sent you / API 401 / Direct URL / Already have key) and 'Choose Your Path' by task (live tools / app code / deliverable / need auth / no-install / no-key fallback). It never assumes a shell or a key; XERJ's 'Start here' assumes both in step 1.
- Feedback is a TOOL inside the protocol with an incentive, not a PR: `firecrawl_search_feedback` refunds 1 credit per search id, says 'Call this after every search you actually use', and asks for a `missingContent` array. Also `POST /support/ask` for agent-to-agent self-recovery. XERJ asks for a git PR, which sandboxed/shell-less agents cannot file.
- Each docs .md page carries a 3-line header pointing back to llms.txt, so an agent that lands on any page can recover the index. XERJ answer pages (per the llms.txt listing) are individual .md files without such a header (inference: not verified page-by-page).
- Secret hygiene stated as a rule for agents: "Never put an API key in the server URL. Never put an API key in an agent chat."
- Pinned versions in install commands (`npx -y firecrawl-mcp@3.23.7`, 'the same reviewed release') and a local `curl http://localhost:3000/health` -> `ok` readiness check.

## What XERJ does better

- XERJ's llms.txt is itself the install doc: commands, PATH gotcha, data-dir gotcha, first-run order, exit codes, progress-line contract are all in the file. Firecrawl's llms.txt is a plain auto-generated Mintlify index with zero commands — every install fact is one or two hops away, and its llms-full.txt is a 1.9 MB unordered concatenation that opens with 'Advanced Scraping Guide'.
- XERJ has a 'Caveats (read before you rely on a capability)' section and honesty rules (lexical default, measured numbers). Firecrawl's docs carry marketing numbers ('Speed up your scrapes by 500%') with no caveats section.
- XERJ documents the failure modes an agent will hit on a human's machine (PATH not edited, data dir inside indexed tree, estimate-before-run, never invent an ETA). Firecrawl's equivalent is a short troubleshooting accordion.
- XERJ ships machine-readable tool schemas from llms.txt (mcp-tools.json, anthropic-tools.json generated from a real tools/list); Firecrawl tells the agent 'Use the schema shown by your MCP client' and does not publish one from llms.txt.
- XERJ's contribution protocol distinguishes issue vs PR vs 'nothing', requires the AI-authored disclosure line, and explains the obligation transfer to the operator. Firecrawl's repo Contributing is four generic lines.
- XERJ verifies the installer's checksum fail-closed and documents a manual verified install; Firecrawl relies on `npx -y ...@latest` with no integrity statement in the agent-facing docs.

## Adoptable ideas

- Publish a skill-format onboarding doc at a stable URL and make the hero prompt literally: "Read and follow https://xerj.org/agent-onboarding/SKILL.md" (Firecrawl: introduction.md, `const prompt = "Read and follow https://www.firecrawl.dev/agent-onboarding/SKILL.md"`). Structure it with Firecrawl's two routers verbatim-style: '**How you might arrive:**' (Docs sent you / got a 401 / direct URL / already installed) and '## Choose Your Path' with an explicit 'Don't want to install anything -> Path E' and 'No shell -> Path F'. Link it from llms.txt line ~10, not from Optional.
- Add a 'For Agents' vs 'For Humans' split (https://docs.firecrawl.dev/mcp-server.md: "For Agents — Start keyless or use an API key." / "For Humans — Sign in via browser."). For XERJ: 'For agents with a shell' (curl | sh) vs 'For agents without a shell' (HTTP-only quickstart + hosted/remote MCP or operator hand-off), so the shell-less persona has a named entry instead of failing at step 1.
- Per-client MCP one-liners on one page, copied from https://docs.firecrawl.dev/mcp-server/keyless.md format: `* Claude Code: \`claude mcp add --transport http firecrawl https://mcp.firecrawl.dev/v2/mcp\`` / `* Codex: \`codex mcp add firecrawl --url ...\`` / `* Cursor or any JSON-config client: {"mcpServers": {...}}` / `* OpenCode (opencode.json): ...`. XERJ equivalent: `claude mcp add xerj -- xerj mcp`, `codex mcp add xerj -- xerj mcp`, and the `.mcp.json` block `xerj init` writes — put these in llms.txt, not only behind `xerj init`.
- Add a '## Verify your connection' + '## Try' pair after MCP setup, modelled on keyless.md: "Open your client's MCP status or tool list and confirm that `firecrawl` is connected. A keyless connection shows `firecrawl_search`, `firecrawl_scrape`, and `firecrawl_parse`" then a fenced text prompt ("Search the web for the latest Firecrawl release notes and summarize the sources.") then "If no Firecrawl tools appear, restart or reload the client after saving its MCP configuration." XERJ: list the ten tool names from mcp-tools.json and give one plain-English `xerj_search` prompt; this also gives a natural place to say 'default search is lexical, not neural' (fixes the 9/12 misread).
- Fix the field-report contradiction by copying Firecrawl's placement logic: the ask sits at the point of use, framed as a normal step with an incentive, never under 'Optional'. Firecrawl README: "**Call this after every search you actually use** (or that didn't help). Bad/partial feedback with `missingContent` is just as valuable as good feedback." and tools.md lists 'Send product feedback' as a Job in the routing table. For XERJ: move the template link out of '## Optional' into 'Start here' step 5 (or rename the heading), and add an MCP tool / HTTP route (`xerj_feedback` / `POST /_xerj/feedback`) so shell-less and sandboxed agents can file without `gh` — Firecrawl's `firecrawl_feedback` tool is the precedent; keep the PR path as the second option.
- Copy Firecrawl's `missingContent`-style structured field into the field-report template: "**Most important field:** `missingContent`. It's an array of specific pieces of content the agent expected to find but did not. One entry per missing topic — these aggregate across teams and tell us what to index next." For XERJ: 'what you searched for and did not find' as the one required field.
- Put a 3-line index-recovery header on every served .md page, verbatim from Firecrawl: "> ## Documentation Index\n> Fetch the complete documentation index at: https://docs.firecrawl.dev/llms.txt\n> Use this file to discover all available pages before exploring further." (XERJ: point at https://xerj.org/llms.txt).
- Add the secret-hygiene rule as a one-liner for agents starting a node with auth: Firecrawl README "Never put an API key in the server URL. Never put an API key in an agent chat. Configure it directly in the client or secret manager." and mcp-server.md "Configure the key through an environment variable or your client's secret storage, never in the MCP URL." — XERJ's `admin.key` / `Authorization: ApiKey` section has no equivalent warning.
- Pin the version in copy-paste install lines the way local.md does (`npx -y firecrawl-mcp@3.23.7` 'the same reviewed release') — XERJ already supports `XERJ_VERSION=vX.Y.Z`; show it inline in the hero command as the reproducible form, and give the `curl http://localhost:9200/_cluster/health` -> expected output readiness check right after the boot command, mirroring "Confirm the server is ready: curl http://localhost:3000/health — The health check returns `ok`."
