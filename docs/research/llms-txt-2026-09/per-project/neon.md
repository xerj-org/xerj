# Neon (Lakebase Postgres, Databricks) — neon.com

Category: database (hosted)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://neon.com/llms.txt> — HTTP 200
- Size: 39737 bytes, 410 lines
- Install position: Two places. (a) Line 9, first bullet of '## Common tasks' (the first section after the header): the unattended-agent provisioning link to https://neon.com/auth.md. (b) Line 76, the one-line preamble of '## Neon CLI': 'Install: `npm i -g neon`. Use this for terminal-first workflows, scripts, and CI/CD automation with `neon`.' There is no dedicated Install section; install pages are link entries (line 122 'Neon CLI: Install and connect', line 104 'Neon CLI command: mcp', line 116 'Neon CLI command: skills', line 41 'Get started with your AI agent').
- Tone: llms.txt: neutral catalogue — an index of links with one-line descriptions; no demands on the agent, no 'you must'. The only imperative is the one-liner 'Install: `npm i -g neon`.' auth.md: terse second-person imperative addressed directly to the agent ('Provision now.', 'Do not run `neon login` on this path.', 'Treat any 2xx as success. Otherwise stop.', 'You are done.') — makes precise demands but each is an operational guard, not an obligation to the vendor. with-an-agent.md: human-facing marketing-lite with an embedded paste-prompt that demands proof ('Prove it works: run a real query ... not just "setup complete."'). MCP README: neutral/technical with a bold security warning.
- Section order: # Neon Postgres (H1 + blockquote description + 'docs available as markdown' note) → ## Common tasks → ## Introduction → ## Get Started → ## Connect → ## Neon CLI → ## AI & Agents → ## Auth (### Quick Start, ### Reference, ### Guides, ### Migrate) → ## Neon Functions → ## Object Storage → ## AI Gateway → ## Data API → ## Branching → ## Manage → ## Guides → ## Import → ## Workflows → ## Reference → ## PostgreSQL → ## Security → ## Extensions → ## Community → ## Concepts → ## Postgres (### Backup Restore) → ## Additional Resources

### Install commands found

```sh
npm i -g neon   (llms.txt line 76, the only literal command in llms.txt)
npm i -g neon@latest   (auth.md)
npm i @neon/config   (auth.md)
neon skills -y -s neon -s neon-postgres   (auth.md)
neon claim create   (auth.md)
neon claim create --service data-api --service auth   (auth.md)
neon claim accept --no-open   (auth.md)
npx neon@latest mcp   (docs/ai/neon-mcp-server.md 'Quick setup')
npx neon@latest plugins   (docs/ai/neon-mcp-server.md)
npx neon@latest init   (docs/ai/neon-mcp-server.md, with-an-agent.md, mcp-server-neon README)
npm install -g neon@latest   (with-an-agent.md agent prompt)
neon plugins --agent <agent>   (with-an-agent.md agent prompt)
neon skills --agent <agent> -s neon -s neon-postgres   (with-an-agent.md agent prompt)
npx add-mcp "https://mcp.neon.tech/mcp?category=projects&category=branches&category=endpoints&category=querying&category=schema"   (README)
npx add-mcp https://mcp.neon.tech/mcp   (README)
npx add-mcp "https://mcp.neon.tech/mcp?category=..." --header "Authorization: Bearer <$NEON_API_KEY>"   (README)
npx neon@latest plugins --agent cursor | --agent claude-code | --agent codex   (llms-full.txt lines 9063-9082)
```

### Text written for an agent (verbatim)

```text
[A] https://neon.com/auth.md (opening, written TO the agent):
# Claimable Neon

You need a Neon project and no human is signed in. Provision now. A human claims it later
if they want to keep it.

This file is https://neon.com/auth.md.

If `NEON_API_KEY` is set, use that account and stop. If `neon` is on `PATH`, run
`neon profile list -o json`. A profile whose `account` is not `-` is an account; use it and
stop. A `DEFAULT` row with `account: "-"` is not an account. Do not run `neon login` on this
path.

[B] https://neon.com/docs/get-started/with-an-agent.md (prompt for a human to PASTE into the agent):
Help me get set up with Neon, based on my project:

1. Install or upgrade the Neon CLI: `npm install -g neon@latest`.
2. Install the Neon agent tooling for your editor, replacing `<agent>` with your editor id (for example `cursor`, `claude-code`, or `codex`): run `neon plugins --agent <agent>` (recommended, installs the Neon plugin), or `neon skills --agent <agent> -s neon -s neon-postgres`. If sign-in opens a browser, ask me to confirm before continuing, and never print secrets.
3. Using the installed Neon skill, create a Neon project (or connect an existing one), link it, and pull my DATABASE_URL into my env file. Add a Postgres driver for my stack.
4. Prove it works: run a real query and show me the result, not just "setup complete." Give me a command to re-check it myself (e.g. `neon psql`).
5. Then suggest next steps, such as a schema or migrations, branching for previews, or Neon's other services (Object Storage, Functions, Managed Better Auth, AI Gateway).

[C] same page, the post-install trigger phrase: ## Tell your agent / In your editor's AI chat, send: / Get started with Neon
```

### MCP snippet (verbatim)

````text
From README (Option 2, OAuth, remote hosted):
```json
{
  "mcpServers": {
    "Neon": {
      "type": "http",
      "url": "https://mcp.neon.tech/mcp?category=projects&category=branches&category=endpoints&category=querying&category=schema"
    }
  }
}
```
From README (Option 3, API key):
```json
{
  "mcpServers": {
    "Neon": {
      "type": "http",
      "url": "https://mcp.neon.tech/mcp?category=projects&category=branches&category=endpoints&category=querying&category=schema",
      "headers": {
        "Authorization": "Bearer <$NEON_API_KEY>"
      }
    }
  }
}
```
llms.txt itself contains NO MCP config snippet; it links to https://neon.com/docs/ai/neon-mcp-server.md and https://neon.com/docs/cli/mcp.md.
````

### Feedback / contribution ask (verbatim)

```text
llms.txt: none (only '- [Docs contribution guide](https://neon.com/docs/community/contribution-guide.md): Learn how to contribute to the Neon documentation' under ## Community, line 385). Per-page .md docs carry this footer (present on with-an-agent.md and neon-mcp-server.md; NOT present in llms.txt, llms-full.txt, or auth.md — grep 'docs-feedback' = 0 hits in all three):

Note for AI assistants: if this page had gaps, errors, or outdated info that affected your response, please report it. POST `{"feedback": "describe the issue", "path": "/docs/get-started/with-an-agent"}` to https://neon.com/api/docs-feedback — no auth required.

README: 'See [CONTRIBUTING.md](./CONTRIBUTING.md) for how to add tools. Tool arguments are `snake_case`.' (line 416). No 'file a report' ask anywhere.
```

### Best passage for driving install and first use (verbatim)

````text
## Provision

Install the CLI, `@neon/config`, and the skills:

```bash
npm i -g neon@latest
npm i @neon/config
neon skills -y -s neon -s neon-postgres
```

The `neon` skill is the Neon platform skill. `neon-postgres` is for working with Lakebase
Postgres after claim.

If you cannot run the CLI, or `neon claim --help` does not list `create`, skip to REST.

[...]

```bash
neon claim create
neon branches list
```

The CLI writes `DATABASE_URL` (and granted service URLs) to `.env` or `.env.local`. Then
skip to Claim when the human is ready.

(source: https://neon.com/auth.md lines 13-50)
````

### One-click links

- `https://cursor.com/en-US/install-mcp?name=Neon&config=eyJ1cmwiOiJodHRwczovL21jcC5uZW9uLnRlY2gvbWNwP2NhdGVnb3J5PXByb2plY3RzJmNhdGVnb3J5PWJyYW5jaGVzJmNhdGVnb3J5PWVuZHBvaW50cyZjYXRlZ29yeT1xdWVyeWluZyZjYXRlZ29yeT1zY2hlbWEifQ%3D%3D   (README badge 'Install MCP Server in Cursor')`
- `https://kiro.dev/launch/mcp/add?name=Neon&config=%7B%22url%22%3A%22https%3A%2F%2Fmcp.neon.tech%2Fmcp%3Fcategory%3Dprojects%26category%3Dbranches%26category%3Dendpoints%26category%3Dquerying%26category%3Dschema%22%7D   (README badge 'Add to Kiro')`
- `https://cursor.com/marketplace/neon   (llms-full.txt line 9063)`
- `Claude connector (no URL): docs/ai/neon-mcp-server.md '## Claude connector — The Neon MCP server is an official Claude connector, so you don't need a custom connector URL.' Settings > Connectors > Browse connectors > Neon`
- `No cursor://, vscode:, or `claude mcp add` strings found in llms.txt, auth.md, the MCP docs page, or the README (grep -i on all four).`

## llms-full.txt

- URL: <https://neon.com/llms-full.txt> — HTTP 200
- Size: 6564134 bytes
- Relationship to llms.txt: Concatenated docs, machine-generated. 135,658 lines, 618 '--- [Document source](URL) ---' separators, each followed by a '> Summary:' line then the page's markdown; 1,179 H1s. Roughly alphabetical by docs path (ai-gateway first), not by llms.txt section order. Contains everything, including page footers stripped of the AI-feedback note. Not curated; 6.5 MB is far beyond a single agent context, so it is realistically only useful for grep/RAG, and the header explicitly points back to llms.txt as the table of contents.
- Install position: No install section of its own. Header (lines 1-5) is the same blockquote as llms.txt plus 'This file contains the full Neon documentation. For a table of contents, see https://neon.com/docs/llms.txt'. First doc concatenated is AI Gateway 'Anthropic Messages API' (alphabetical by path), so install material is deep: first `npx neon@latest init` at line 9109; the with-an-agent.md paste-prompt at line 65279; auth.md URL mentioned 10 times, claimable REST flow around line 126999. Neither the auth.md text nor the per-page 'Note for AI assistants' feedback footer is included (0 hits for 'docs-feedback').

## Other agent-facing files

### auth.md (Claimable Neon — unattended-agent provisioning) — HTTP 200

<https://neon.com/auth.md>

162-line file written to an agent that has no signed-in human: decision tree (env var → CLI profile → provision), CLI path with skills install, explicit no-shell REST fallback (POST claimable.neon.tech/v1/agent/identity → token → credentials), then a Claim handoff protocol with polling and secret-discard rules. Linked as the first bullet of llms.txt '## Common tasks'.

```text
If you cannot run the CLI, or `neon claim --help` does not list `create`, skip to REST.
[...]
- `project.id`, `project.branch_id`, and `project.expires_at`. The unclaimed project dies at
  `expires_at` (72 hours today).
[...]
Register returns 201 today. Treat any 2xx as success. Otherwise stop. Do not continue with
missing fields.
[...]
At `reconciled: true`, discard the identity assertion, access tokens, and pre-claim
`database_url`. They no longer authorize project access. Auth and the Data API stay enabled
and transfer with the project if they were granted. The human owns the project. You are
done.
```

### Neon MCP Server overview (docs page as markdown) — HTTP 200

<https://neon.com/docs/ai/neon-mcp-server.md>

220 lines. Every .md docs page starts with a breadcrumb + index pointer + '> Summary:' line for agents. Quick setup = three CLI commands (`npx neon@latest mcp`, `plugins`, `init`); Claude connector (official, no URL); access-control URL params (?readonly=true, ?projectId=, ?category=); security guidance; tool catalogue. Ends with the AI-assistant feedback footer.

````text
> This page location: Postgres > Connect to Postgres > Connection methods > Overview
> Full Neon documentation index: https://neon.com/docs/llms.txt

> Summary: The Neon MCP Server implements the Model Context Protocol (MCP), letting AI assistants interact with your Neon projects on your behalf. Set up with `npx neon@latest mcp` or use the config generator. Supports OAuth and API key auth.
[...]
## Quick setup

Install the Neon MCP server into your coding agents with [`neon mcp`](https://neon.com/docs/cli/mcp):

```bash
npx neon@latest mcp
```

It prompts for where to write the config, which agents to install into, and how to authenticate, then writes it for you.
[...]
Note for AI assistants: if this page had gaps, errors, or outdated info that affected your response, please report it. POST `{"feedback": "describe the issue", "path": "/docs/ai/neon-mcp-server"}` to https://neon.com/api/docs-feedback — no auth required.
````

### mcp-server-neon README — HTTP 200

<https://raw.githubusercontent.com/neondatabase/mcp-server-neon/main/README.md>

494 lines. Two one-click badges at top (Cursor deeplink, Kiro), three example natural-language prompts, a bold security warning, then three numbered setup options (init / OAuth remote / API-key remote) each with an `npx add-mcp` one-liner AND a verbatim JSON config, per-client sections, read-only/scoping params, tool list. No feedback/report ask; CONTRIBUTING.md pointer only.

````text
### Option 1. Quick Setup with API Key

**Don't want to manually create an API key?**

Run [`neon@latest init`](https://neon.com/docs/cli/init) to automatically configure Neon's MCP Server with one command:

```bash
npx neon@latest init
```

This works with Cursor, VS Code (GitHub Copilot), and Claude Code. It will authenticate via OAuth, create a Neon API key for you, and configure your editor automatically.

### Option 2. Remote Hosted MCP Server (OAuth Based Authentication)
[...]
```bash
npx add-mcp "https://mcp.neon.tech/mcp?category=projects&category=branches&category=endpoints&category=querying&category=schema"
```
````

### Get started with your AI agent (with-an-agent.md) — HTTP 200

<https://neon.com/docs/get-started/with-an-agent.md>

86 lines. The human-facing onboarding page whose centre is a 5-step prompt to paste into the agent (verbatim above), plus the manual alternative (`npx neon@latest init`) and a one-line trigger phrase 'Get started with Neon' to send after manual install. Linked from llms.txt line 41.

````text
4. Prove it works: run a real query and show me the result, not just "setup complete." Give me a command to re-check it myself (e.g. `neon psql`).
[...]
## Tell your agent

In your editor's AI chat, send:

```text
Get started with Neon
```

Your agent reads the installed skill to create or connect a Neon project, pull your `DATABASE_URL` into your env file, add a Postgres driver, and run a real query to confirm the connection.
````

### Neon CLI command: mcp (cli/mcp.md) — HTTP 200

<https://neon.com/docs/cli/mcp.md>

148 lines. The CLI writes agents' MCP configs itself; interactive by default, fully flag-driven for headless/agent use; supports 15 named agents; scoping flags; warns that minted keys are account-wide.

```text
The supported agents are `antigravity`, `cline`, `cline-cli`, `claude-code`, `codex`, `cursor`, `gemini-cli`, `goose`, `github-copilot-cli`, `grok-build`, `mcporter`, `opencode`, `vscode`, `windsurf`, and `zed`. A few names also accept aliases (`claude` for `claude-code`, `copilot` for `vscode`, `gemini` for `gemini-cli`). Passing an unknown agent stops the command and prints the current supported list.
```

### neondatabase/agent-skills README (SKILL.md host repo) — HTTP 200

<https://raw.githubusercontent.com/neondatabase/agent-skills/main/README.md>

170 lines. Catalogue of installable skills (neon, neon-postgres, neon-auth, neon-postgres-agent-platforms, branches, egress-optimizer, object-storage, ...) each with a skills.sh install-count badge. Skills are the mechanism by which the agent learns Neon after install.

```text
It all starts with the `SKILL.md` file in the skill's directory. It's the entry point and allows agents to progressively discover information as needed.
[...]
An overview of Neon for apps and agents — Lakebase Postgres, Auth, Data API, Object Storage, Compute Functions, and the AI Gateway — and how to get started. Includes Claimable Neon for use with no Neon account.
```

### neon.tech/llms.txt and llms-full.txt — HTTP 200

<https://neon.tech/llms.txt>

Redirect to neon.com equivalents (url_effective = https://neon.com/llms.txt, 410 lines identical). https://neon.com/docs/llms.txt also serves the same 410-line file.

```text
(identical content to https://neon.com/llms.txt)
```

### neondatabase/neon AGENTS.md — HTTP 404

<https://raw.githubusercontent.com/neondatabase/neon/main/AGENTS.md>

Does not exist. CLAUDE.md at the same path is also 404.

### llms-install.md — HTTP 404

<https://neon.com/llms-install.md>

Does not exist (404 HTML shell, 9 lines).

## What they do better than XERJ

- A first-class unattended-agent path (auth.md) that is the first link on the index, written in second-person imperative to the agent, with an explicit 'cannot run the CLI → REST' fallback and hard stop conditions. XERJ buries the no-shell path as the 4th link under 'Start here' and its Core operations assume a running local node the agent had to start with a shell.
- Install is one command that also configures the agent (`npx neon@latest init` / `neon mcp` / `neon plugins`) and it supports 15 named agents with a printed list on unknown input; XERJ's `xerj init` is step 3 and marked '(optional, one command)'.
- The paste-prompt demands proof ('not just "setup complete"') and a re-check command for the human — XERJ's hero prompt does not require the agent to demonstrate a real hit back to the human.
- Feedback path is a no-auth HTTP POST printed on every page as a 'Note for AI assistants' footer — zero PR/CLA/gh dependency, so a sandboxed agent can actually comply. XERJ's report requires git+gh or a handoff to the operator.
- Per-page machine headers (location breadcrumb, index URL, one-line Summary) so a page read out of context still points back to the index.
- MCP install documented four ways (CLI writer, `npx add-mcp`, JSON for two config shapes, one-click badges, official Claude connector) plus read-only / project-scoped / category-scoped URL params that a cautious agent can pick without asking.
- Sub-indexes: 'All 33 Introduction pages' links keep llms.txt at 40 KB while remaining complete.

## What XERJ does better

- XERJ's llms.txt is curated and opinionated (Start here → Caveats → Core operations → Answers), whereas Neon's llms.txt is a plain generated docs index apart from the '## Common tasks' block and one 'Install:' line; it contains no prompt, no MCP snippet, no caveats, no feedback ask.
- XERJ front-loads honesty caveats ('## Caveats (read before you rely on a capability)', the PATH-consequence warning, exit-code semantics, data-dir-inside-indexed-tree trap); Neon's index has no equivalent and its per-page warnings are only in the linked pages.
- XERJ names the actual agent operations and ships tool schemas (mcp-tools.json, anthropic-tools.json) from the index; Neon's index links to pages about tools but exposes no schema file.
- XERJ's llms-full.txt (84 KB) is a curated, sectioned document (e.g. '§10 run-it-for-a-human protocol') that fits a context window; Neon's llms-full.txt is a 6.5 MB alphabetical concatenation of 618 pages that no agent can load whole and that drops the per-page feedback footer.
- XERJ asks for and specifies the field report (template, filename rule, 25-line ceiling, 'a negative report is a good report', sandbox handoff rule); Neon asks agents for nothing beyond page-level docs feedback and its README has no contribution ask at all.
- XERJ documents the manual verified install without `curl | sh` and the SHA-256 fail-closed behaviour; Neon's index has no supply-chain statement (its install is `npm i -g neon`).

## Adoptable ideas

- Put the unattended/no-human path as the FIRST bullet of the first section. Neon llms.txt line 9: `- [Need a Neon account and the user is not around: provision one now; they claim it later](https://neon.com/auth.md)`. XERJ equivalent: `- [No shell, no human at the keyboard: reach a node over HTTP only](https://xerj.org/docs/agents/quickstart.md)` as the first line under '## Start here' — directly addresses the 22-run finding that shell-less personas fail at every tier.
- Write a single agent-addressed decision-tree file with an explicit no-shell branch, in Neon's auth.md register: `If you cannot run the CLI, or \`neon claim --help\` does not list \`create\`, skip to REST.` XERJ version: 'If you cannot run a process, skip to HTTP. Everything under Core agent operations is `POST http://localhost:9200/...`; only `xerj autoindex` needs a shell.' Keep auth.md's guard style: 'Treat any 2xx as success. Otherwise stop. Do not continue with missing fields.'
- Ship a copy-paste human→agent prompt that ends with proof, not a status line. Neon with-an-agent.md step 4: `Prove it works: run a real query and show me the result, not just "setup complete." Give me a command to re-check it myself (e.g. \`neon psql\`).` XERJ: add to the hero prompt 'Prove it works: run `xerj search "<a term you saw in my repo>"` and show me the file:line hit, then give me the one command to re-check it myself.' This also creates the natural moment to say which embedder is running (fixes the 9/12 'misread lexical as neural').
- Make feedback a zero-friction, no-auth, no-PR HTTP POST and print it on every doc page as a footer addressed to the agent — Neon's exact wording: `Note for AI assistants: if this page had gaps, errors, or outdated info that affected your response, please report it. POST \`{"feedback": "describe the issue", "path": "/docs/ai/neon-mcp-server"}\` to https://neon.com/api/docs-feedback — no auth required.` XERJ's field-report ask currently requires gh + a CLA-exempt PR; a `POST https://xerj.org/api/field-report` with `{agent, used_for, verdict, version}` (and `xerj feedback` posting to it by default, `--open-pr` as the upgrade) removes the reason sandboxed agents drop the obligation. Note Neon strips this footer from llms.txt/llms-full.txt — XERJ can do better by keeping it in both.
- Give every agent-facing .md page a 3-line machine header: `> This page location: Start with Neon > One-command setup` / `> Full Neon documentation index: https://neon.com/docs/llms.txt` / `> Summary: ...`. XERJ answer pages could carry `> Index: https://xerj.org/llms.txt` and a one-line `> Summary:` so a page fetched in isolation still routes the agent back.
- Use `npx add-mcp <url>` / the agent's own CLI to WRITE the MCP config rather than showing JSON only. Neon README: `npx add-mcp "https://mcp.neon.tech/mcp?category=projects&category=branches&category=endpoints&category=querying&category=schema"` and `neon mcp --agent claude-code --yes`. XERJ has `xerj init`; document it in llms.txt as `xerj init --agent claude-code -y` style non-interactive form and list the supported agent ids the way cli/mcp.md does ('Passing an unknown agent stops the command and prints the current supported list').
- State the markdown-availability rule in the llms.txt header so agents stop fetching HTML: Neon line 5: `Neon docs are available as markdown. Append \`.md\` to any doc URL or set \`Accept: text/markdown\`. This is the primary index. Sections with many pages show key pages and link to full sub-indexes.` XERJ already serves .md answers; say so in one sentence at the top.
- Add one-click MCP install badges (Cursor deeplink + Kiro) to the MCP README and reference them from llms.txt. Neon: `https://cursor.com/en-US/install-mcp?name=Neon&config=<base64 {"url":...}>` and `https://kiro.dev/launch/mcp/add?name=Neon&config=<urlencoded json>`. For XERJ's stdio server the Cursor config would be base64 of `{"command":"xerj","args":["mcp"]}`.
