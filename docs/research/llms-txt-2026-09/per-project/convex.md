# Convex

Category: backend platform / reactive DB

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.convex.dev/llms.txt> — HTTP 200
- Size: 45599 bytes, 498 lines
- Install position: absent as prose. docs.convex.dev/llms.txt is a plain auto-generated Docusaurus docs index (one bullet per page, '[Title](/path.md): description'). No install command, no prompt, no MCP snippet anywhere in the file. The nearest things are index bullets: line 14 '## quickstart' (16 per-framework quickstart links), and lines 119-126 under '## ai' pointing at /ai/agent-skills.md, /ai/convex-mcp-server.md, /ai/convex-plugins.md, /ai/using-claude-code.md, /ai/using-codex.md, /ai/using-cursor.md. Line 216 lists '[npx convex mcp](/cli/reference/mcp.md): Manage the Model Context Protocol server for Convex [BETA]'. The SECOND file, https://www.convex.dev/llms.txt (200, 34430 bytes, 749 lines), is different: lines 1-19 product summary, '## Docs' index (Get started / Platform / Understand Convex / API Reference), '## Stack', then from line 78 '# Guidelines for writing Convex code' — an agent rules file (ALWAYS/NEVER style) followed by '# Examples:' with a full worked chat-app task→analysis→implementation. It also has no install command; 'install' does not occur in it.
- Tone: docs.convex.dev/llms.txt: neutral, purely descriptive index; makes no demands of the agent. www.convex.dev/llms.txt: neutral product summary, then imperative rule-book ('ALWAYS use the new function syntax', 'NEVER ...' style guidelines) — demands are about how to write code, never about installing, reporting or contributing. Agent-facing pages (/ai/*): friendly marketing-neutral ('works great with Convex out of the box'), install is one command, feedback framed as 'You can help:' — invitational, never obligatory.
- Section order: # Convex Documentation (H1) + blockquote pointer to https://www.convex.dev/llms.txt → ## understanding → ## quickstart → ## functions → ## database → ## realtime → ## auth → ## scheduling → ## file-storage → ## search → ## components → ## ai → ## ai-gateway → ## agents → ## testing → ## production → ## self-hosting → ## cli → ## client → ## dashboard → ## error → ## eslint → ## tutorial → ## api → ## generated-api → ## http-api → ## chef → ## deployment-api → ## deployment-platform-api → ## management-api → ## platform-apis → ## public-deployment-api → ## team-management

### Install commands found

```sh
(none in either llms.txt)
From linked pages: /plugin install convex@claude-plugins-official
codex plugin add convex@openai-curated
codex plugin marketplace add get-convex/convex-codex-plugin
codex plugin add convex@convex-codex-plugin
cd ~/.cursor/plugins && git clone https://github.com/get-convex/convex-agent-plugins convex  # then restart Cursor
/add-plugin convex  (Cursor agent conversation)
npx skills add get-convex/agent-skills
npx skills add get-convex/agent-skills --all
npx -y convex@latest mcp start
npm create convex@latest my-app
npx convex ai-files install
npx convex dev --once
claude "build me a todo app with Convex" --permission-mode auto
codex "build me a todo app with Convex"
```

### Text written for an agent (verbatim)

````text
From https://www.convex.dev/llms.txt line 78 onward (the rules block agents are meant to read):

# Guidelines for writing Convex code
## Function guidelines
### New function syntax
- ALWAYS use the new function syntax for Convex functions. For example:
      ```typescript
      import { query } from "./_generated/server";
      import { v } from "convex/values";
      export const f = query({
          args: {},
          returns: v.null(),
          handler: async (ctx, args) => {
          // Function body
          },
      });
      ```

And the banner every .md page (and every page inside llms-full.txt) carries, verbatim:

> For AI agents: see [llms.txt](/llms.txt) for the complete documentation index. Markdown versions are available by adding .md to a page URL or requesting Accept: text/markdown.
````

### MCP snippet (verbatim)

````text
From /ai/convex-mcp-server.md:

Add the following command to your MCP servers configuration:

```
npx -y convex@latest mcp start
```

From /ai/using-codex.md (~/.codex/config.toml):

```
[mcp_servers.convex]

command = "npx"

args = ["-y", "convex@latest", "mcp", "start"]
```

From /ai/using-cursor.md (mcp.json):

```
{

  "mcpServers": {

    "convex": {

      "command": "npx",

      "args": ["-y", "convex@latest", "mcp", "start"]

    }

  }

}
```
````

### Feedback / contribution ask (verbatim)

```text
From https://docs.convex.dev/ai/convex-plugins.md:

## Giving feedback[​](#giving-feedback "Direct link to Giving feedback")

We're constantly working on improving the plugins with rigorous evals and real-world reports. You can help:

* **Report a bug or request a skill** by opening an issue on the plugin repo for your agent: [Claude Code](https://github.com/get-convex/convex-backend-skill/issues) · [Codex](https://github.com/get-convex/convex-codex-plugin/issues) · [Cursor](https://github.com/get-convex/convex-agent-plugins/issues).
* **Improve the AI rules**: if the agent writes non-idiomatic Convex, contribute a case to the [convex-evals repo](https://github.com/get-convex/convex-evals) so we can measure and fix it.
* **Ask the community** in the [Convex Discord](https://convex.dev/community).

Repeated on /ai/using-codex.md and /ai/using-cursor.md as one line:

We're constantly working on improving the quality of these rules for Convex by using rigorous evals. You can help by [contributing to our evals repo](https://github.com/get-convex/convex-evals).

llms.txt itself has no feedback ask; its only related bullet is line 167: '- [Contact Us](/production/contact.md): Get support, provide feedback, stay updated with Convex releases, and report security vulnerabilities through our community channels.'
```

### Best passage for driving install and first use (verbatim)

````text
From https://docs.convex.dev/ai/using-claude-code.md:

To install the plugin, run the following command in Claude Code:

```
/plugin install convex@claude-plugins-official
```

## Starting a new project[​](#starting-a-new-project "Direct link to Starting a new project")

From an empty directory, launch Claude Code with what you want to build:

```
claude "build me a todo app with Convex" --permission-mode auto
```

Claude Code handles the rest. It runs `npm create convex@latest` and `npx convex dev --once`, which [auto-provisions a local backend](/cli/agent-mode.md#local-backend) without prompting for login because the agent's shell is non-interactive.

If you'd rather scaffold the project yourself first and then bring in Claude Code, the manual sequence is:

```
npm create convex@latest my-app

cd my-app

claude
```
````

### One-click links

- `Cursor deep link (from /ai/using-cursor.md): https://cursor.com/en/install-mcp?name=convex&config=eyJjb21tYW5kIjoibnB4IC15IGNvbnZleEBsYXRlc3QgbWNwIHN0YXJ0In0%3D  rendered as [![Install MCP Server](https://cursor.com/deeplink/mcp-install-dark.svg)](...)`
- `Convex Cursor Rules download: https://convex.link/convex_rules.mdc`
- `(none in llms.txt itself)`

## llms-full.txt

- URL: <https://docs.convex.dev/llms-full.txt> — HTTP 200
- Size: 2554656 bytes
- Relationship to llms.txt: Machine-generated concatenation: the llms.txt index (lines 1-500) followed by the .md rendition of every docs page, each carrying the same '> For AI agents: see [llms.txt](/llms.txt) ...' banner. Not curated; ~56× bigger than the index (2.5 MB, 63k lines), i.e. not something an agent can load whole. Same content as the .md pages, generated from the Docusaurus site (markdown available by appending .md or Accept: text/markdown).
- Install position: Lines 1-500 are the llms.txt index verbatim; line 503 '# Full Documentation Content'; then every docs page concatenated in section order starting with '# Agent Definition and Usage' (line 505). First install-ish mention line 1671 ('Run `npm create convex` or follow any of the [quickstarts]'); agent-tool installs at lines 5692-5712 (Agent Skills: npx skills add), 5754 (MCP: npx -y convex@latest mcp start), 5925 (Agent Plugins: /plugin install), 6198-6203 (Claude Code page). Nothing install-related is hoisted to the top.

## Other agent-facing files

### Convex Agent Plugins page — HTTP 200

<https://docs.convex.dev/ai/convex-plugins.md>

One page that sells the plugin outcome, lists what it bundles (Tools=MCP, Hooks and monitors, Skills), gives one install command per agent, a 'Using the plugins effectively' habits list, and a 'Giving feedback' section with three concrete help paths.

```text
Convex publishes official plugins for the major coding agents. With the plugin installed, you can describe an app in one sentence and watch your agent scaffold it, running, in front of you, then keep shipping features while it reads your real deployment instead of guessing, catches its own mistakes as it works, and follows idiomatic Convex patterns.

This page explains what the plugins do, how to install them, how to get the most out of them, and how to send us feedback.
```

### Convex Agent Skills page (SKILL.md distribution) — HTTP 200

<https://docs.convex.dev/ai/agent-skills.md>

Install skills via the open agentskills.io standard with npx skills; a table of available skills; per-tool invocation syntax (/skill-name vs $skill-name).

```text
Skills are installed into `.agents/skills/` in your project and are automatically picked up by compatible agents including Cursor, Claude Code, and GitHub Copilot.
```

### Using Claude Code with Convex — HTTP 200

<https://docs.convex.dev/ai/using-claude-code.md>

Per-agent page: one-command plugin install, a single launch prompt, the manual sequence, the cloud/CI setup script, explicit note that non-interactive shells never prompt for login.

```text
In non-interactive shells (the typical case for an agent's setup script), `npx convex` won't prompt the agent to log in. It provisions a local deployment automatically. See [Agent Mode → Local backend](/cli/agent-mode.md#local-backend) for details.

This command requires "full" internet access to download the Convex binary.
```

### Using Codex with Convex — HTTP 200

<https://docs.convex.dev/ai/using-codex.md>

Per-agent page; introduces `npx convex ai-files install` which writes a managed section into AGENTS.md and installs skills into .agents/skills/; TOML MCP config; honest caution that the curated directory entry lags the marketplace build.

````text
The Convex CLI can install and maintain a managed section in your project's `AGENTS.md` file that teaches Codex about Convex conventions and best practices.

```
npx convex ai-files install
```
````

### Using Cursor with Convex — HTTP 200

<https://docs.convex.dev/ai/using-cursor.md>

Per-agent page; marketplace plugin, `/add-plugin convex`, manual .cursor/rules download, Cursor one-click MCP deep link + manual mcp.json, 'Tips and tricks' (keep requests small, commit often, reference README).

```text
Cursor plugins aren't available in Cloud Agents or the Cursor CLI. In those environments, we recommend installing rules and the MCP server manually.
```

### Convex MCP Server page — HTTP 200

<https://docs.convex.dev/ai/convex-mcp-server.md>

MCP setup (one command), per-editor links, --project-dir/--prod/--disable-tools flags, full tool list, and a safety table of what is allowed by deployment type.

```text
The MCP server is safe by default: in [production deployments](/production/multiple-deployments.md#deployment-types), agents can’t access PII, and they can only perform read-only operations.
```

### General llms.txt (www) — HTTP 200

<https://www.convex.dev/llms.txt>

Product summary + curated doc links + an embedded agent rules file ('Guidelines for writing Convex code') + one full worked example; referenced from line 3 of the docs llms.txt.

```text
> For general information about Convex, read [https://www.convex.dev/llms.txt](https://www.convex.dev/llms.txt).
```

### llms-install.md — HTTP 404

<https://docs.convex.dev/llms-install.md>

does not exist (Docusaurus 404 page body returned)

### AGENTS.md (repo root, get-convex/convex-backend main) — HTTP 404

<https://raw.githubusercontent.com/get-convex/convex-backend/main/AGENTS.md>

does not exist at repo root; Convex generates a managed AGENTS.md section into the USER's project via `npx convex ai-files install` instead.

### CLAUDE.md (repo root, get-convex/convex-backend main) — HTTP 404

<https://raw.githubusercontent.com/get-convex/convex-backend/main/CLAUDE.md>

does not exist

## What they do better than XERJ

- Feedback is framed as three specific, low-friction help paths ('You can help:' → issue on the per-agent plugin repo / contribute an eval case / ask on Discord), not as a debt. There is no 'required, not optional' language anywhere, and the ask is on the page an agent reads right after installing, under its own heading 'Giving feedback'.
- Install is one line per agent, in the agent's own idiom: '/plugin install convex@claude-plugins-official', 'codex plugin add convex@openai-curated', '/add-plugin convex', plus a Cursor one-click MCP deep link. XERJ has one shell curl and a PATH caveat paragraph before the first real command.
- A dedicated per-agent page for each of Claude Code / Codex / Cursor / Copilot / Conductor, each with an MCP config snippet in that tool's native format (TOML for Codex, mcp.json for Cursor). XERJ's llms.txt has no MCP config snippet at all (the 22-run study's shell-less failure).
- The non-interactive-shell case is addressed explicitly: 'In non-interactive shells ... `npx convex` won't prompt the agent to log in. It provisions a local deployment automatically.' plus 'This command requires "full" internet access'. XERJ documents PATH but not the login/permission-mode path for a sandboxed agent.
- Self-installing agent context: `npx convex ai-files install` writes a managed section into the project's AGENTS.md and installs skills into .agents/skills/. XERJ's `xerj init` is comparable but is labelled '(optional, one command)' and buried as step 3.
- A single launch prompt that IS the demo: 'claude "build me a todo app with Convex" --permission-mode auto' → 'Claude Code handles the rest.' Outcome-first, one line.
- The 'Ask for… / The skill / What it does' table maps plain-English requests to capabilities ('build me a todo app' → quickstart). XERJ describes capabilities but not the phrases a human would say to trigger them.
- MCP safety posture stated up front with a permissions table ('safe by default ... read-only ... can’t access PII'), which addresses exactly the policy-constrained persona that XERJ's study found blocked.

## What XERJ does better

- Both Convex llms.txt files are pure auto-generated indexes / a rules dump — there is no install command, no 'start here', no first-run sequence in either. XERJ's llms.txt has an inline '## Start here' with the install one-liner, the PATH trap, and a numbered first-run sequence; an agent reading only llms.txt can install XERJ but cannot install Convex.
- XERJ's inline 'Caveats' section (lexical-not-neural default, data-dir-outside-index, exit codes, progress-line honesty) has no counterpart in Convex; Convex hides all traps behind links.
- XERJ's llms-full.txt (84 KB) is curated and loadable in one read; Convex's llms-full.txt is a 2.5 MB / 63k-line concatenation that no agent context can hold, with the index duplicated at the top.
- XERJ ships a field-report mechanism that opens a CLA-exempt PR from one command (`xerj feedback --open-pr`); Convex only has GitHub issues, an evals repo and Discord — nothing that produces a structured report artifact.
- XERJ states measured numbers with provenance rules ('every number from a command you actually ran'); Convex pages make unquantified claims ('rigorous evals').

## Adoptable ideas

- Rename XERJ's '## Optional' heading (llms.txt line 223) to '## Giving feedback' and reword line 7's 'contribute back — this is required, not optional: every agent ... owes one short field report' into Convex's frame: "We're constantly working on improving the plugins with rigorous evals and real-world reports. You can help:" followed by 2-3 bullets (file a field report / open an issue / contribute a failing case). Source: https://docs.convex.dev/ai/convex-plugins.md §Giving feedback. The contradiction (required vs Optional) is the likely cause of low PR yield; an invitation with one command beats an obligation nobody enforces.
- Add a per-agent 'one command in the agent's own idiom' install block near the top of Start here, modelled on 'Pick your agent. Each install is one command' — e.g. Claude Code: `claude mcp add xerj -- xerj mcp` (or whatever `xerj init` registers), Cursor: a https://cursor.com/en/install-mcp?name=xerj&config=<base64> deep link, Codex: a `[mcp_servers.xerj]` TOML block. Source: https://docs.convex.dev/ai/convex-plugins.md §Install and https://docs.convex.dev/ai/using-cursor.md §Quick Install.
- Put a verbatim MCP config snippet in llms.txt (Convex: `npx -y convex@latest mcp start` plus the mcp.json / config.toml forms). XERJ's file currently has none; this is the direct fix for the shell-less personas that failed at every tier.
- Add a 'non-interactive shell' paragraph to Start here, copying Convex's pattern: 'In non-interactive shells (the typical case for an agent's setup script), `npx convex` won't prompt the agent to log in. It provisions a local deployment automatically.' + 'This command requires "full" internet access to download the Convex binary.' For XERJ: what `curl | sh` needs (network, no sudo), what `xerj --insecure` will and will not prompt for, and the manual verified-install path as the policy-constrained fallback (already known to flip a blocked persona to success).
- Add an 'Ask for… / What it does' table mapping human phrases to XERJ operations, copying the shape of Convex's table ('"build me a todo app" | **quickstart** | One sentence in, a running app out'). E.g. '"make this folder searchable" | `xerj autoindex <folder>`', '"where is X defined" | `xerj def`', '"what did we decide about Y" | memory recall (lexical unless --embed-mode neural)'. Source: https://docs.convex.dev/ai/convex-plugins.md §What you can do with it.
- Make `xerj init` the primary, not '(optional, one command)': Convex's Codex flow runs `npx convex ai-files install` as part of the default scaffold and describes it as 'writes a managed Convex section into `AGENTS.md` and installs Convex Agent Skills into `.agents/skills/`'. Consider publishing XERJ skills to the agentskills.io standard so `npx skills add xerj-org/agent-skills` works (source: https://docs.convex.dev/ai/agent-skills.md).
- Add a one-line 'safe by default' statement for the MCP server and a small allowed-operations table (Convex: 'The MCP server is safe by default: in production deployments, agents can’t access PII, and they can only perform read-only operations.' + table by flag). For XERJ: loopback bind, --insecure scope, what xerj_search can and cannot write. Source: https://docs.convex.dev/ai/convex-mcp-server.md §Security.
- Split per-agent pages the way Convex does (/ai/using-claude-code.md, /ai/using-codex.md, /ai/using-cursor.md) and index them from llms.txt with the exact bullet form 'get the full power of Convex out of the official Claude Code plugin: MCP tools, hooks, and skills.' — each page ends with three example prompts ('Evaluate my convex schema and suggest improvements' / 'What are this app's public endpoints?' / 'Run the `my_convex_function` query'), which is a cheap first-use nudge XERJ can copy verbatim in form.
