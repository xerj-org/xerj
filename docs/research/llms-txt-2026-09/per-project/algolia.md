# Algolia

Category: search engine (hosted, closed-source core; llms.txt is on the marketing domain, docs are Mintlify)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://www.algolia.com/llms.txt> — HTTP 200
- Size: 7866 bytes, 188 lines
- Install position: Lines 7-10, the FIRST section after the description (## Build with AI). It contains no command; it delegates to a separate agent-facing page: 'Fetch and follow the instructions at https://www.algolia.com/doc/guides/get-started/build-with-ai.md'. Nothing else in llms.txt is about installation; the lower 2/3 (lines 54-188) is a live search API for the algolia.com website content itself (endpoint + public API key + filter recipes).
- Tone: llms.txt: neutral/marketing catalogue (link list + product blurbs) with one short second-person invitation ('Want to set up...? Fetch and follow...'). The linked build-with-ai.md 'Agent setup' block is strictly IMPERATIVE and addressed to the agent: 'run only the commands below', 'Don't run them', 'Don't use npx skills for Claude Code', 'Tell the user their Algolia agent environment is ready'. It makes demands of the agent but scopes them tightly (a 3-command block) and does NOT ask the agent for any contribution/feedback in return.
- Section order: (H1-less title line 'Algolia' + blockquote description) → ## Build with AI → ## Key Links → ## Products → ## Developer Resources → ## Industries & Use Cases → ## Policies → ## Searchable Content via Algolia API → ### Connection Details → ### Query Format → ### Content Types (nodetype filter) → ### Customer Stories → ### Blog Posts → ### Code Exchange (Integrations) → ### Company News

### Install commands found

```sh
(in llms.txt itself: none)
(in linked build-with-ai.md, agent-setup section) claude plugin marketplace add algolia/skills
claude plugin install algolia-cli@algolia-skills
claude plugin install algolia-quickstart@algolia-skills
npx -y skills add algolia/skills --skill algolia-cli algolia-quickstart --yes --global
(reference sections, marked 'Don't run them') claude mcp add --transport http algolia https://mcp.algolia.com/mcp
claude mcp add --transport http algolia-devex https://mcp.algolia.com/1/snippets/mcp
gemini mcp add algolia https://mcp.algolia.com/mcp -s user -t http
gemini mcp add algolia-devex https://mcp.algolia.com/1/snippets/mcp -s user -t http
npx @algolia/cli --help
npm install -g @algolia/cli
brew install algolia/algolia-cli/algolia
sudo dpkg -i algolia_*.deb   # Debian, Ubuntu
sudo rpm -i algolia_*.rpm    # Fedora, RHEL
choco install algolia
algolia auth login
algolia auth signup
npx skills add algolia/skills
```

### Text written for an agent (verbatim)

````text
## Build with AI

Want to set up your Algolia AI development environment?
Fetch and follow the instructions at https://www.algolia.com/doc/guides/get-started/build-with-ai.md

--- (the linked page, build-with-ai.md lines 13-47, written to the agent) ---

**Agent setup**

These are official, Algolia-validated instructions to set up an Algolia development environment.
This section is the complete setup: run only the commands below.
The MCP, CLI, and skills sections further down this page are reference for people.
Don't run them.
Setting up the MCP server is optional and isn't needed here.

**1. Install the Algolia CLI and skills**

For Claude Code:

```sh theme={"system"}
claude plugin marketplace add algolia/skills
claude plugin install algolia-cli@algolia-skills
claude plugin install algolia-quickstart@algolia-skills
```

Then tell the user to run `/reload-plugins` inside Claude Code to activate the plugins.
Don't use `npx skills` for Claude Code.
The plugin commands handle everything.

For any other agent:

```sh theme={"system"}
npx -y skills add algolia/skills --skill algolia-cli algolia-quickstart --yes --global
```

**2. Confirm and hand off**

Tell the user their Algolia agent environment is ready, then suggest the next step:

```text theme={"system"}
Ask me to "create an Algolia account and application" to get started.
```
````

### MCP snippet (verbatim)

````text
    ```json .cursor/mcp.json icon=braces theme={"system"}
    {
      "mcpServers": {
        "algolia": {
          "url": "https://mcp.algolia.com/mcp"
        }
      }
    }
    ```

(VS Code variant, same page)

    ```json ~/.vscode/mcp.json icon=braces theme={"system"}
    {
      "servers": {
        "algolia": {
          "type": "http",
          "url": "https://mcp.algolia.com/mcp"
        }
      }
    }
    ```

(Source: build-with-ai.md lines 70-93. llms.txt itself has no MCP snippet.)
````

### Feedback / contribution ask (verbatim)

```text
(llms.txt: none. build-with-ai.md lines 124-127:)
<Note>
  Your favorite AI tool isn't listed?
  [Reach out to Algolia support](https://support.algolia.com/hc/en-us/requests/new) and request it.
</Note>
```

### Best passage for driving install and first use (verbatim)

````text
**Agent setup**

These are official, Algolia-validated instructions to set up an Algolia development environment.
This section is the complete setup: run only the commands below.
The MCP, CLI, and skills sections further down this page are reference for people.
Don't run them.
Setting up the MCP server is optional and isn't needed here.

**1. Install the Algolia CLI and skills**

For Claude Code:

```sh theme={"system"}
claude plugin marketplace add algolia/skills
claude plugin install algolia-cli@algolia-skills
claude plugin install algolia-quickstart@algolia-skills
```

Then tell the user to run `/reload-plugins` inside Claude Code to activate the plugins.

**2. Confirm and hand off**

Tell the user their Algolia agent environment is ready, then suggest the next step:

```text theme={"system"}
Ask me to "create an Algolia account and application" to get started.
```
````

### One-click links

- `cursor://anysphere.cursor-deeplink/mcp/install?name=algolia&config=eyJ1cmwiOiJodHRwczovL21jcC5hbGdvbGlhLmNvbS9tY3AifQ%3D%3D  (build-with-ai.md line 66, 'Add Algolia MCP to Cursor (requires Cursor 1.0+)')`
- `cursor://anysphere.cursor-deeplink/mcp/install?name=algolia-devex&config=eyJ1cmwiOiJodHRwczovL21jcC5hbGdvbGlhLmNvbS8xL3NuaXBwZXRzL21jcCJ9  (line 165)`
- `claude mcp add --transport http algolia https://mcp.algolia.com/mcp  (line 59)`
- `claude mcp add --transport http algolia-devex https://mcp.algolia.com/1/snippets/mcp  (line 160)`
- `vscode:mcp/install?<json> generated by getVSCodeInstallHref in productivity-mcp.md (MDX helper, rendered as a button on the HTML page)`
- `(llms.txt itself contains no one-click links)`

## llms-full.txt

- URL: <https://www.algolia.com/llms-full.txt> — HTTP 404
- Size: 11530 bytes
- Relationship to llms.txt: No relationship at the marketing root (llms-full.txt is 404 there). On the docs domain, /doc/llms.txt (200, 41 lines, 2,729 bytes) is a Mintlify-generated index: 5 section bundles under /_llms/<section>.md with page counts ('Guides (573 pages)', 'Libraries and tools (616 pages)', ...), 15 OpenAPI spec links, and a recursion instruction: '> The links below point to documentation indexes. Follow each `/_llms/` index recursively until you reach documentation pages.' /doc/llms-full.txt is the auto-concatenated full corpus (13.8 MB), not curated. The root llms.txt is hand-written and separate from both; it links to /doc/llms.txt as 'agent consumable navigation for the markdown version of the docs'.
- Install position: absent (404 at marketing root). A docs-domain variant DOES exist: https://www.algolia.com/doc/llms-full.txt -> 200, 13,802,990 bytes, 399,711 lines, 2,063 '# ' page headings. It is a Mintlify concatenation of every docs page (each block = '# <title>' + 'Source: <url>' + raw MDX incl. <Card>/<Columns> tags). Install content appears only wherever the build-with-ai / CLI pages happen to fall in that concatenation; there is no curated install-first ordering.

## Other agent-facing files

### docs index (Mintlify) llms.txt — HTTP 200

<https://www.algolia.com/doc/llms.txt>

Auto-generated docs navigation: 5 section bundles with page counts, 15 OpenAPI YAML specs, recursive sub-indexes. Nothing about installation.

```text
> The links below point to documentation indexes. Follow each `/_llms/` index recursively until you reach documentation pages.
```

### build-with-ai.md (agent setup page, markdown) — HTTP 200

<https://www.algolia.com/doc/guides/get-started/build-with-ai.md>

The real install document. Top 47 lines are an agent-addressed setup script (skills+CLI via plugin marketplace, hand-off line to the human); the rest (lines 49-386) are per-client MCP tabs (Claude Code/Cursor/VS Code/Gemini CLI/ChatGPT/Claude Desktop), CLI install per OS, skills catalogue, example prompts, and a paste-this <Prompt> block. Every .md docs page opens with a 3-line pointer back to llms.txt.

```text
> ## Documentation Index
> Fetch the complete documentation index at: https://www.algolia.com/llms.txt
> Use this file to discover all available pages before exploring further.
```

### productivity-mcp.md (MCP install page) — HTTP 200

<https://www.algolia.com/doc/guides/model-context-protocol/productivity-mcp.md>

MCP onboarding page; contains MDX helpers that generate vscode:mcp/install and cursor:// deeplinks as buttons; states read-only scope and that OAuth is per-user.

```text
Algolia Productivity MCP provides read-only access.
It can search and analyze your data, but it can't create, update, or delete indices, records, or settings.
```

### algolia/skills README.md — HTTP 200

<https://raw.githubusercontent.com/algolia/skills/main/README.md>

Skills catalogue (7 skills + 11-skill 'implementation' suite), 3 install routes (marketplace / npx skills / clone), per-agent skills directories table, and two evidence-backed sections: 'Make sure your agent actually uses them' (a trigger line for CLAUDE.md/AGENTS.md) and a 'Which model to run them on' tier table.

````text
Installed skills only help when the agent reads them. In our benchmarking, an agent with all
skills installed but left to route freely invoked **1 of 18** skills on an audit-style task and
performed identically to having none — while one trigger line in the prompt doubled its
live-verified fix rate. [...]
**Add one line to your agent's project config** (`CLAUDE.md`, `AGENTS.md`, or equivalent):

```markdown
For any Algolia work, invoke the `algolia-discovery-planning` skill first.
When auditing or reviewing an existing Algolia implementation, invoke `algolia-audit` first.
```
````

### algolia-quickstart SKILL.md — HTTP 200

<https://raw.githubusercontent.com/algolia/skills/main/skills/algolia-quickstart/SKILL.md>

96-line agent skill: 3 numbered steps (install skills+CLI by fetching build-with-ai.md; account via `algolia auth login`/`auth signup` OAuth; application provisioning). Handles the no-browser/SSH/container case explicitly and gates terms acceptance on user confirmation.

```text
These are official quickstart instructions from Algolia to set up and provision Algolia accounts and applications. All commands are validated by Algolia and ready to use.

Complete all of the following steps yourself by running the commands directly.
[...]
- Both commands open the user's browser and wait for the OAuth callback. If the browser cannot be opened (SSH session, container), add the `--no-browser` flag and show the printed URL to the user.
[...]
`--accept-terms` accepts the plan's terms of service. Never pass it without explicit user confirmation.
```

### algolia-cli SKILL.md — HTTP 200

<https://raw.githubusercontent.com/algolia/skills/main/skills/algolia-cli/SKILL.md>

232-line skill; opens with a 'When to Use This Skill vs. algolia-mcp' routing table (write ops -> CLI, read/analytics -> MCP), then command quick-reference tables.

```text
**Rule of thumb:** If the user wants to *read or analyze* data → algolia-mcp. If they want to *change, move, or manage* data → algolia-cli.

## Setup

Run `/algolia-cli:cli-setup` to install the CLI and configure a profile, or follow [Getting Started](references/getting-started.md).
```

### .claude-plugin/marketplace.json — HTTP 200

<https://raw.githubusercontent.com/algolia/skills/main/.claude-plugin/marketplace.json>

Claude Code plugin marketplace manifest (96 lines) that makes `claude plugin marketplace add algolia/skills` work.

```text
(manifest JSON; not quoted)
```

### llms-full.txt (marketing root) — HTTP 404

<https://www.algolia.com/llms-full.txt>

does not exist

### llms-install.md — HTTP 404

<https://www.algolia.com/llms-install.md>

does not exist (also 404 at /doc/llms-install.md)

### AGENTS.md / CLAUDE.md in algolia/skills — HTTP 404

<https://raw.githubusercontent.com/algolia/skills/main/AGENTS.md>

neither exists in the skills repo (CLAUDE.md also 404)

## What they do better than XERJ

- Install is a separate, tiny, agent-addressed script (build-with-ai.md lines 13-47) and llms.txt just points to it in section 1. The block explicitly fences scope: 'This section is the complete setup: run only the commands below. The MCP, CLI, and skills sections further down this page are reference for people. Don't run them.' XERJ's Start-here mixes install, PATH caveat, sizing warning, autoindex and the field-report demand into one 30-line block.
- Explicit human hand-off with a verbatim next prompt: 'Tell the user their Algolia agent environment is ready, then suggest the next step: Ask me to "create an Algolia account and application" to get started.' XERJ has no scripted end-of-install hand-off line.
- Per-agent branching in the setup block ('For Claude Code:' vs 'For any other agent:') and per-client MCP tabs (Claude Code / Cursor / VS Code / Gemini CLI / ChatGPT / Claude Desktop) with cursor:// and vscode: deeplinks plus JSON config files. XERJ documents `xerj init` writing .mcp.json but has no per-client table and no deeplinks.
- A shell-less path exists: remote HTTP MCP (`https://mcp.algolia.com/mcp`) configurable from ChatGPT/Claude Desktop connector settings with no terminal at all. XERJ's persona study shows shell-less agents fail; Algolia's doc addresses that persona (though XERJ is a local binary, so the analogue is documenting `xerj mcp` config JSON for each client, not a hosted URL).
- Every .md docs page starts with the same 3-line pointer back to llms.txt ('Fetch the complete documentation index at ... Use this file to discover all available pages before exploring further') — hop-1 discovery from any entry page.
- Skill-based distribution with a Claude Code plugin marketplace manifest (`claude plugin marketplace add algolia/skills`), and a measured note that installed skills are ignored unless a one-line trigger is put in CLAUDE.md/AGENTS.md ('invoked 1 of 18 skills ... one trigger line ... doubled its live-verified fix rate').
- No-browser/SSH/container fallback spelled out in the skill ('add the `--no-browser` flag and show the printed URL to the user') and a consent gate ('Never pass it without explicit user confirmation').
- llms.txt tail turns the company's own site into a queryable API for the agent (endpoint, public search key, nodetype filters, example JSON bodies) instead of a static link list — 'This allows you to search for specific content rather than relying on static lists.'

## What XERJ does better

- XERJ llms.txt is self-contained: real install commands (`curl -fsSL https://xerj.org/get | sh`, `irm https://xerj.org/get.ps1 | iex`) are inline; Algolia's llms.txt has zero commands and requires a second fetch.
- XERJ has an inline Caveats section that corrects the most common agent misreadings (lexical-vs-neural default, `--embed-mode` applies at ingest, `_passage` projection, RSS sizing); Algolia's files have no honesty/caveat layer at all and describe NeuralSearch only as marketing.
- XERJ has an explicit contribution/feedback protocol with a one-command path (`xerj feedback --open-pr ...`, `--dry-run` for sandboxed agents, operator hand-off). Algolia asks nothing back from agents; its only CTA is a support-ticket link for unlisted tools.
- XERJ publishes machine-readable tool schemas (mcp-tools.json, anthropic-tools.json) generated from a real tools/list; Algolia publishes OpenAPI specs but no MCP tool schema listing.
- XERJ documents security-relevant facts an agent must relay (checksum fail-closed install, HuggingFace download provenance, no pinned checksum); Algolia's agent block asserts 'official, Algolia-validated' without verifiable detail.
- XERJ's llms-full.txt is curated (1,235 lines) rather than a 13.8 MB concatenation of every docs page.

## Adoptable ideas

- Split the agent install into its own short, fenced, agent-addressed page and point to it from llms.txt section 1, copying the framing verbatim from https://www.algolia.com/doc/guides/get-started/build-with-ai.md lines 13-19: 'These are official, <X>-validated instructions to set up ... This section is the complete setup: run only the commands below. The ... sections further down this page are reference for people. Don't run them.' For XERJ: e.g. https://xerj.org/docs/agents/setup.md with exactly install -> PATH export -> start node -> autoindex -> one search -> hand-off, and llms.txt line ~9 becoming 'Want to set up XERJ for this agent? Fetch and follow the instructions at https://xerj.org/docs/agents/setup.md'.
- Add a scripted hand-off line, modelled on build-with-ai.md lines 41-47: 'Tell the user their XERJ environment is ready, then suggest the next step:' followed by a text block the agent is meant to show verbatim, e.g. 'Ask me to "index this repository and search it" to get started.' — and put the field-report ask INSIDE that same hand-off step ('...then run `xerj feedback --dry-run` and show the user the draft') so it is executed at the natural end of setup instead of being filed under 'Optional'.
- Branch by agent inside the setup block exactly as Algolia does ('For Claude Code:' / 'For any other agent:' — build-with-ai.md lines 23-39), and add per-client MCP config tabs with the JSON shapes verbatim from lines 70-93 adapted to a stdio server: `.cursor/mcp.json` -> {"mcpServers":{"xerj":{"command":"xerj","args":["mcp"]}}} and `~/.vscode/mcp.json` -> {"servers":{"xerj":{"type":"stdio",...}}}. Pair with `claude mcp add xerj -- xerj mcp` and a cursor://anysphere.cursor-deeplink/mcp/install?name=xerj&config=<base64 json> deeplink. This directly targets the shell-less/MCP-only persona failure from the 22-run study.
- Prepend the 3-line discovery banner Algolia puts on every .md page (build-with-ai.md lines 1-3) to every xerj.org/answers/*.md, /compare/*.md and /docs/*.md: '> ## Documentation Index\n> Fetch the complete documentation index at: https://xerj.org/llms.txt\n> Use this file to discover all available pages before exploring further.'
- Ship XERJ as a Claude Code plugin/skill with a marketplace manifest (`claude plugin marketplace add xerj-org/skills` / `claude plugin install xerj@xerj-skills`) and `npx -y skills add xerj-org/skills --yes --global` for other agents, so 'install' for an agent is one line that also installs the MCP registration; and copy the README's evidence-backed trigger-line advice: 'Add one line to your agent's project config (CLAUDE.md, AGENTS.md, or equivalent): For any <X> work, invoke the `<skill>` skill first.'
- Copy the no-browser/sandbox fallback pattern from algolia-quickstart SKILL.md line 60 ('If the browser cannot be opened (SSH session, container), add the `--no-browser` flag and show the printed URL to the user.') into the field-report step: XERJ already has `--dry-run`; state it as a numbered branch in the setup script rather than a parenthetical.
- Fix the naming contradiction directly: Algolia labels the optional thing 'Setting up the MCP server is optional and isn't needed here.' and the mandatory thing 'run only the commands below'. XERJ should rename '## Optional' (line 223) to something like '## Your side of the deal' and move the field-report template link under Start here step 4 (line 24), which already says 'required'.
- Expose the xerj.org corpus as a queryable endpoint in llms.txt the way Algolia does (lines 54-79: endpoint, method, headers, query JSON, 'Always include ...' rule) — a public read-only XERJ node over the docs/answers/recipes, with a verbatim curl body, so an agent can search instead of scrolling a 234-line link list. (Inferred as valuable; XERJ currently lists links only.)
