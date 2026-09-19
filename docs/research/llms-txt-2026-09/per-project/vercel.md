# Vercel

Category: cloud platform (hosted) — the platform llms.txt, separate from the AI SDK row

Dissected 2026-09-19 by fetching the live URLs below with `curl -sL`; every verbatim block was sliced from the fetched body by script, and each page's key quotes are re-checked in ../proposals/FACTCHECK.md. The comparison notes are a reading, not a measurement.

## llms.txt

- URL: <https://vercel.com/llms.txt> — HTTP 200
- Size: 4726 bytes, 64 lines
- Install position: Line 25, inline in a link description — not a code line. '## Agent setup' (third H2, line 21) is four links; one link description contains an inline command: 'Install with `npx plugins add vercel/vercel-plugin`'. The playbook with the real commands is get-started.md, the first link.
- Tone: Addressed to agents by heading ('## How agents should use Vercel'). Three bullets, each 'For <task>, <do this>'. One restraint rule: 'Ask for approval before changing account resources.' No obligation toward the vendor.
- Section order: # Vercel → ## When to use Vercel → ## How agents should use Vercel → ## Agent setup → ## Documentation → ## REST API and developer resources → ## Self-serve onboarding → ## Knowledge Base → ## Learning and updates → ## Optional

### Install commands found

```sh
npx plugins add vercel/vercel-plugin  (llms.txt, inside a link description)
(get-started.md) npm install --global vercel@latest
(get-started.md) npx skills add vercel-labs/agent-skills --global
(get-started.md) vercel mcp --clients "<client>"
(get-started.md) claude mcp add --transport http vercel --scope user https://mcp.vercel.com
(get-started.md) codex mcp add vercel --url https://mcp.vercel.com
(vercel-mcp.md) npx -y add-mcp https://mcp.vercel.com -g
```

### Text written for an agent (verbatim)

```text
## How agents should use Vercel

- For research, fetch the Markdown documentation indexes below and follow their links to individual Markdown pages.
- For REST API calls, read the OpenAPI description and authentication documentation before choosing an operation. Ask for approval before changing account resources.
- For Vercel MCP, use an OAuth-capable MCP client and let the user authorize access to their Vercel account.
```

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
## Agent setup

- [Set up Vercel for your coding agent](https://vercel.com/get-started.md): Follow the agent playbook to install the CLI, add Vercel guidance, and connect Vercel MCP.
- [Getting started with Vercel](https://vercel.com/docs/getting-started-with-vercel.md?from=llms-txt): Install the Vercel CLI, add the Vercel plugin or Vercel Skills, and deploy your first project.
- [Vercel plugin](https://vercel.com/docs/agent-resources/vercel-plugin.md?from=llms-txt): Install with `npx plugins add vercel/vercel-plugin` for Vercel skills, commands, and specialist agents.
- [Vercel Skills](https://vercel.com/docs/agent-resources/skills.md?from=llms-txt): Browse and install individual skills for coding agents that do not support the plugin.
```

## llms-full.txt

- URL: <https://vercel.com/docs/llms-full.txt> — HTTP 200
- Size: 9509006 bytes
- Relationship to llms.txt: Linked from llms.txt as 'Full documentation content'. 9,509,006 bytes, 207,571 lines. TRAP: https://vercel.com/llms-full.txt (no /docs/) answers HTTP 200 with a 2.6 MB HTML application shell, as do https://vercel.com/SKILL.md and https://vercel.com/ai-catalog.json — a status code alone would record three files that do not exist.
- Install position: The file's second line routes agents to the plugin before any documentation ('Set up a supported coding agent with the [Vercel plugin]…').

## Other agent-facing files

### get-started.md (agent playbook) — HTTP 200

<https://vercel.com/get-started.md>

6527 bytes. Front matter lists `supported_surfaces` and `runtimes`. An execution contract, check-before-install commands, plugin-or-skills (not both), per-client MCP with config paths for Cursor, Windsurf and Gemini CLI, a verification sequence, and a fixed completion block.

````text
## How to run

Run section 1 once per machine. Run sections 2 and 3 once per coding agent. In section 2, install either the Vercel Plugin or standalone skills based on what the current agent supports.

For Section 3, add the Vercel MCP, identify the current runtime and follow only its matching MCP route.

Perform actions yourself when terminal or file access is available. Global or user-scoped installation is the default. Pause only for user authentication, approval, or UI actions you cannot perform. Do not merely return commands for the user to copy.

[...]

### Common authorization and verification

Before authorizing, verify the endpoint is exactly:

```text
https://mcp.vercel.com
```

When a browser or consent screen opens, pause for the user to complete OAuth.

After authorization:

1. Call `search_vercel_documentation`.
2. Call the authenticated, read-only `list_teams` tool.
3. Refresh the client's MCP tools or server list if either is unavailable.
4. Start or resume a session, or reload or restart the client, only if refreshing does not load the server.

Keep human confirmation enabled for every MCP mutation. Connecting MCP gives the agent the access of the authorizing Vercel user.

## Completion

Report only verified state:

```text
▲ Vercel agent setup is ready
CLI: <version>, authenticated as <username>
Guidance: <plugin|skills|skipped>, global or user scope
MCP: <connected|skipped>, global shared endpoint, https://mcp.vercel.com
MCP config: <path|managed by Vercel CLI>
Authenticated MCP check: <list_teams succeeded|not requested>
Reload: <not needed|completed>
```
````

### docs/agent-resources/vercel-mcp.md — HTTP 200

<https://vercel.com/docs/agent-resources/vercel-mcp.md>

15870 bytes. Leads with a one-command installer for every detected client, then eleven per-client sections.

````text
## Quick Setup

Use the `add-mcp` CLI to add the Vercel MCP to all your tools.

```bash
npx -y add-mcp https://mcp.vercel.com -g
```
````

### .well-known/agent-skills/index.json — HTTP 200

<https://vercel.com/.well-known/agent-skills/index.json>

5846 bytes, schema discovery/0.2.0, 9 skills of `type: archive` whose `url` is a GitHub release tarball. /.well-known/skills/index.json is 404 (9-byte 'Not Found').

### agents.md — HTTP 200

<https://vercel.com/agents.md>

1914 bytes — the Markdown twin of a marketing page ('Build and run intelligent agents on Vercel'), not an agent instruction file. A name collision worth knowing about before fetching `/agents.md` on any site.

## What they do better than XERJ

- The whole file is 4,726 bytes and the agent route is the third heading. llms.txt is a router; the playbook is a separate 6.5 KB file with one job.
- `## Optional` holds only machine catalogues an agent may skip — the use the convention intends.
- The playbook's execution contract is one paragraph: 'Perform actions yourself when terminal or file access is available. … Pause only for user authentication, approval, or UI actions you cannot perform. Do not merely return commands for the user to copy.'
- It tells the agent to merge, not overwrite: 'Before changing a MCP configuration file, inspect it and merge the `vercel` entry without replacing unrelated settings.'
- It ends with a fixed block under 'Report only verified state:' whose every line has an enumerated value set ('<connected|skipped>', '<not needed|completed>').
- Per-client config paths are stated for clients XERJ's docs never mention: Windsurf (`~/.codeium/windsurf/mcp_config.json`, key `serverUrl`) and Gemini CLI (`~/.gemini/settings.json`).

## What XERJ does better

- XERJ's install command and first-run sequence are in llms.txt itself; Vercel's llms.txt needs one hop for any command.
- XERJ's llms-full.txt is curated and loadable; Vercel's is 9.5 MB.
- XERJ's install has an integrity-checked path; the playbook's is `npm install --global vercel@latest`.
- xerj.org returns a real 404 for files that do not exist. vercel.com answers 200 with an HTML shell for /SKILL.md and /llms-full.txt, which misleads a status-code probe.

## Adoptable ideas

- Split the router from the playbook: llms.txt routes in its first screen; an install playbook with one job lives at its own URL.
- Adopt the completion block's form — a fixed heading, one line per fact, enumerated values — and the sentence above it: 'Report only verified state:'.
- Adopt the merge rule verbatim in spirit for `.mcp.json`, `.cursor/mcp.json` and friends. `xerj init` already merges; hand-written config steps must say so too.
- Add Windsurf and Gemini CLI to the per-client table with their file paths.
- Keep `## Optional` for skippable links only.
