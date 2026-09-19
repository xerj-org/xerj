# Exa

Category: agent tooling (web search API)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://exa.ai/docs/llms.txt (fetched via https://docs.exa.ai/llms.txt, 301 -> 200)> — HTTP 200
- Size: 24309 bytes, 168 lines
- Install position: No install command in the file itself. The nearest thing is lines 4-6 (the 2nd-4th bullets): the quickstart, Exa MCP and Agent Skills links. It is a Mintlify auto-generated docs index (link + one-line description per page); every install command lives behind the links. Note a SECOND, hand-written llms.txt exists at the site root https://exa.ai/llms.txt (200, 154 lines, 7,116 bytes) with sections '## MCP Server' and '## Agent Skills' placed FIRST, before Products/Pricing/Comparisons.
- Tone: neutral / catalogue. docs/llms.txt is a pure link index with descriptive one-liners; it makes NO demands of the agent (no 'you must', no 'required'). The root exa.ai/llms.txt is lightly marketing ("**Start for free:** New accounts receive $20 in free credits...") but still addresses the agent directly once: "For AI agents and automation, start with the agent-native interfaces below." The imperative voice appears only inside the copyable setup prompt on the skill pages, and there it is written as the USER instructing the agent ("Install the skill FIRST", "Hard rule throughout: the key is a secret"), not the vendor instructing the agent.
- Section order: # Exa (no preamble, no blockquote summary; lines 3-162 are one flat bullet list of 160 page links in nav order: Get started (index, quickstart, Exa MCP, Agent Skills, 3 skill pages) -> Search API/Deep Search/Highlights/Snapshot/Best practices/Data Index verticals -> Exa Agent + Exa Connect providers -> Contents/Monitors/Batch/SDK quickstart -> Integrations (ChatGPT/Codex, Claude, OpenAI SDK, tool-calling, Vercel, Stripe Projects, x402/MPP/Nevermined payments, Slack, n8n, ... Browserbase) -> Admin (pricing, billing, team, security, EMA, status, error codes, OpenAPI, FAQs) -> Websets -> API reference endpoints -> Changelog → ## OpenAPI Specs (3 links: exa-spec.json, exa-spec.yaml, team-management-spec.yaml)

### Install commands found

```sh
(none inside docs/llms.txt itself)
behind the links, verbatim from the linked pages: `npx skills add exa-labs/agent-skills`
`npx skills add exa-labs/agent-skills --skill "build-with-exa"` (and --skill "exa-search", --skill "exa-contents")
`claude plugin install exa@claude-plugins-official`
`claude mcp add --transport http exa https://mcp.exa.ai/mcp`
`codex mcp add exa --url https://mcp.exa.ai/mcp` (exa-mcp-server README)
`pip install exa-py` / `npm install exa-js` / `uv add exa-py`
```

### Text written for an agent (verbatim)

```text
(from https://exa.ai/docs/get-started/agent-skills/build-with-exa.md, the 'Option B: Copy this prompt into your coding agent' block; truncated to 25 lines, the full prompt is ~30 lines and ends with a 'Hard rule throughout' paragraph)
Set up the Exa build-with-exa agent skill on this machine.

Goal:
- Install the build-with-exa skill so my coding agent can use it to build applications and agents with Exa's full API platform.
- Get an Exa API key working WITHOUT ever exposing, printing, or pasting the key into this chat.

Selected agent:
- Claude Code, Codex, Cursor, or any Agent-Skills-compatible agent
- Global install directories: ~/.claude/skills (Claude Code), ~/.codex/skills (Codex), ~/.agents/skills (Cursor / other)
- Project-local install directories: .claude/skills (Claude Code), .agents/skills (Codex / Cursor / other)

Skill source:
- SKILL.md URL: https://raw.githubusercontent.com/exa-labs/agent-skills/main/skills/build-with-exa/SKILL.md

What to do:
1. Install the skill FIRST, before any key setup. Prefer a project-local install when working inside a repo; otherwise use the matching global directory listed above. Create the chosen skills directory and download the skill:
   mkdir -p <skills-dir>/build-with-exa && curl -fsSL "https://raw.githubusercontent.com/exa-labs/agent-skills/main/skills/build-with-exa/SKILL.md" -o <skills-dir>/build-with-exa/SKILL.md
   Then verify that <skills-dir>/build-with-exa/SKILL.md exists.
2. Check whether an Exa API key is already available FROM YOUR OWN COMMAND-RUNNING ENVIRONMENT — use the same tool/shell you will run the skill with, not by asking me to echo it. The skill resolves the key from EXA_API_KEY first, then from the file ~/.config/exa/key, so check both without ever printing a value:
   printf '%s\n' "${EXA_API_KEY:+env-set}"; [ -s ~/.config/exa/key ] && printf 'file-set\n'
4. Smoke-test the key from your own shell — resolve it from the env var or the file, and print only the status code:
   KEY="${EXA_API_KEY:-$(cat ~/.config/exa/key 2>/dev/null)}"
   curl -s -o /dev/null -w "%{http_code}\n" -X POST https://api.exa.ai/search \
     -H "Authorization: Bearer $KEY" -H "Content-Type: application/json" \
     -d '{"query":"exa.ai","numResults":1}'
   Keep the endpoint, headers, and body exactly as written (do not guess the schema). It must return 200, not 401/429.
5. Tell me how to restart or rescan my agent so it discovers the skill.
```

### MCP snippet (verbatim)

````text
(from https://exa.ai/docs/get-started/exa-mcp.md, 'Other clients' tab)
Most clients use the standard `mcpServers` shape:

```json theme={null}
{
  "mcpServers": {
    "exa": {
      "url": "https://mcp.exa.ai/mcp"
    }
  }
}
```

(and, same page, the local-package variant)
```json theme={null}
{
  "mcpServers": {
    "exa": {
      "command": "npx",
      "args": ["-y", "exa-mcp-server"],
      "env": {
        "EXA_API_KEY": "your_api_key"
      }
    }
  }
}
```
````

### Feedback / contribution ask (verbatim)

```text
NONE in docs/llms.txt, none in root exa.ai/llms.txt, none in the skills/MCP pages. The only feedback-shaped text in llms-full.txt is support routing, e.g. (Error Codes page, line 230): "* Contact [hello@exa.ai](mailto:hello@exa.ai) with the response status, error body, and `requestId`." No field-report, issue, or PR ask exists anywhere in the agent-facing files.
```

### Best passage for driving install and first use (verbatim)

````text
(https://exa.ai/docs/get-started/agent-skills/overview.md, lines 13-33)
## Install

Install every Exa skill at once:

```bash theme={null}
npx skills add exa-labs/agent-skills
```

<Card title="Get your Exa API key" icon="key" horizontal href="https://dashboard.exa.ai/api-keys">
  Create a key in the dashboard. New accounts start with free credits.
</Card>

<Note>
  Set your key as `EXA_API_KEY` in your agent environment.
</Note>

Or open a skill page below and copy its setup prompt into your agent. The prompt installs that skill and verifies your API key without printing it.

## Skills

Each skill page includes a one-line description, a copyable setup prompt, and a link to the raw `SKILL.md` source.
````

### One-click links

- `cursor://-equivalent: https://cursor.com/en/install-mcp?name=exa&config=eyJ1cmwiOiJodHRwczovL21jcC5leGEuYWkvbWNwIn0= (exa-mcp-server README badge 'Install in Cursor')`
- `https://vscode.dev/redirect/mcp/install?name=exa&config=%7B%22type%22%3A%22http%22%2C%22url%22%3A%22https%3A%2F%2Fmcp.exa.ai%2Fmcp%22%7D (README badge + MCP docs page 'one-click install')`
- `https://claude.com/plugins/exa (Claude Plugin Marketplace badge)`
- `https://chatgpt.com/plugins/exa?open_in_app (Codex/ChatGPT plugin badge)`
- `https://lmstudio.ai/install-mcp?name=exa&config=eyJ1cmwiOiJodHRwczovL21jcC5leGEuYWkvbWNwIn0%3D`
- `https://cursor.com/marketplace/exa`
- `https://claude.ai/connectors/exa`
- `claude mcp add --transport http exa https://mcp.exa.ai/mcp`
- `codex mcp add exa --url https://mcp.exa.ai/mcp`
- `npx skills add exa-labs/agent-skills`

## llms-full.txt

- URL: <https://exa.ai/docs/llms-full.txt (fetched via https://docs.exa.ai/llms-full.txt, 301 -> 200)> — HTTP 200
- Size: 766571 bytes
- Relationship to llms.txt: Mechanically generated (Mintlify) concatenation of every page listed in docs/llms.txt, each as its raw MDX '.md twin' with a 'Source: <url>' line under the H1. Same content as the individual .md pages, including unrendered <Card>/<Tabs>/<Steps>/<img> JSX and even the docs-home radio-button markup (line ~7520). Not curated, no agent preamble, 779 headings. Each individual .md page (not llms-full) is prefixed with a 3-line agent banner: '> ## Documentation Index / > Fetch the complete documentation index at: https://exa.ai/docs/llms.txt / > Use this file to discover all available pages before exploring further.' — that banner is absent from llms-full.txt (grep count 0).
- Install position: Installation content appears only where the concatenated source pages carry it: 'Build with Exa Skill' at line 6687 (npx skills add --skill at 6707, copyable prompt 6720-6750), 'Exa Contents Skill' 6767, 'Exa Search Skill' 6847, 'Agent Skills' overview 6927 (npx skills add at 6941), 'Exa MCP' 6985 (mcpServers snippets at 7101/7130/7156/7169), 'Developer quickstart' 7359 (pip/npm at 7424-7428), 'Exa in Claude Code, Web, and Desktop' 8378 (claude mcp add at 8496). That is ~35% of the way through a 19,818-line file; the file OPENS with 'Billing and Rate Limits' (line 1), 'Error Codes' (105), 'FAQs' (233), 'Enterprise Managed Auth' (294) because pages are concatenated in folder-alphabetical order (admin/, agent/, batch/, changelog, contents/, get-started/, integrations/, ...).

## Other agent-facing files

### Agent Skills overview — HTTP 200

<https://exa.ai/docs/get-started/agent-skills/overview.md>

Hub page: one-command install of all skills, API-key note, and the 'copy its setup prompt into your agent' alternative for every skill; 59 lines

```text
Or open a skill page below and copy its setup prompt into your agent. The prompt installs that skill and verifies your API key without printing it.
```

### Build with Exa Skill page (per-skill page with copyable prompt) — HTTP 200

<https://exa.ai/docs/get-started/agent-skills/build-with-exa.md>

Per-skill page: Option A `npx skills add ... --skill`, Option B a ~30-line prompt the human pastes into the agent; prompt installs via mkdir+curl into the right skills dir per agent, probes for the key without printing it, smoke-tests with a 200 check, then tells the user how to rescan

```text
Hard rule throughout: the key is a secret. Only ever inspect it via a presence/length check (`${EXA_API_KEY:+set}`, `[ -s ~/.config/exa/key ]`) or an HTTP status code — never print, `echo`, `cat`, or `grep`-with-output any file or variable that may contain it, and never try to "redact" a key file with a regex. If a key is ever exposed, tell me to rotate it at https://dashboard.exa.ai/api-keys.
```

### Exa MCP install page — HTTP 200

<https://exa.ai/docs/get-started/exa-mcp.md (https://exa.ai/docs/reference/exa-mcp.md redirects here)>

Per-client install tabs (ChatGPT/Codex, Claude, Grok Build, Cursor, VS Code, Other clients) with one-click links, JSON configs, a 9-row 'where the config lives / URL key name' table, three auth modes (keyless/OAuth/API key), tool list, troubleshooting accordions; 375 lines

````text
Exa offers a hosted server that works in any MCP client:

```text theme={null}
https://mcp.exa.ai/mcp
```

No API key is required to get started. Exa MCP is open source and available on [GitHub](https://github.com/exa-labs/exa-mcp-server).
````

### agent-skills SKILL.md (exa-search) — HTTP 200

<https://raw.githubusercontent.com/exa-labs/agent-skills/main/skills/exa-search/SKILL.md>

The skill body itself (287 lines): YAML frontmatter name/description with 'Use when ...' trigger phrasing, then cURL quick-start with exact request bodies

```text
description: "Call Exa Search directly with cURL or raw HTTP. Use when an agent needs Exa semantic web retrieval from POST /search without an SDK, including ranked results, domain or category filters, freshness-aware result content, highlights or text extraction, structured output, or streaming search responses."
```

### exa-mcp-server skills/search/SKILL.md (orchestrator skill) — HTTP 200

<https://raw.githubusercontent.com/exa-labs/exa-mcp-server/main/skills/search/SKILL.md>

A 207-line agent-role skill shipped inside the MCP repo: auth prerequisites in priority order, then a research-orchestration procedure; notable for telling the agent what to do on auth failure

```text
On auth / rate-limit errors, surface the fix (prefer OAuth) — don't fall back to generic web search.
```

### exa-mcp-server README — HTTP 200

<https://raw.githubusercontent.com/exa-labs/exa-mcp-server/main/README.md>

Four one-click install badges (Cursor, VS Code, Claude Plugin, Codex/ChatGPT Plugin) at the top, hosted URL first, then per-client CLI commands and an 11-client config-location table; 133 lines

```text
Invoke from your client's skill UI (or `/skill-name` where supported). MCP-only setups still get the tools; skills add orchestration on top.
```

### agent-skills README — HTTP 200

<https://raw.githubusercontent.com/exa-labs/agent-skills/main/README.md>

26-line README: skills.sh badge, API-key note, 5-skill table, one install command

```text
> You will need an API key to use the skills.
> You can get an API key from the [Exa Dashboard](https://dashboard.exa.ai/), then set it as `EXA_API_KEY` in your agent environment.
```

### Root-site llms.txt (hand-written, separate from docs index) — HTTP 200

<https://exa.ai/llms.txt>

154-line curated llms.txt with a blockquote summary, then sections in this order: MCP Server, Agent Skills, Documentation, Products, Use Cases, Integrations, SDKs, OpenAPI, Pricing, Evals and Benchmarks, Comparisons, Customers, Research, Company and Trust

```text
For AI agents and automation, start with the agent-native interfaces below. For complete technical documentation, use the Exa documentation index.
```

### exa-mcp-server llms-install.md — HTTP 404

<https://raw.githubusercontent.com/exa-labs/exa-mcp-server/main/llms-install.md>

does not exist

### exa-mcp-server AGENTS.md / CLAUDE.md; agent-skills AGENTS.md / CLAUDE.md — HTTP 404

<https://raw.githubusercontent.com/exa-labs/exa-mcp-server/main/AGENTS.md (and CLAUDE.md; same for exa-labs/agent-skills)>

none of the four exist

### docs llms-install.md — HTTP 404

<https://exa.ai/docs/llms-install.md>

does not exist

## What they do better than XERJ

- Shell-less agents are a first-class path: the hosted MCP URL `https://mcp.exa.ai/mcp` works with 'No API key is required to get started', plus one-click install links for Cursor/VS Code/LM Studio, a Claude connector and plugin, and a ChatGPT/Codex plugin. An MCP-only persona has a complete route with zero terminal. XERJ's llms.txt says the MCP server is wired by `xerj init` (a CLI step) and the Agent quickstart is 'HTTP-only ... except xerj autoindex, which is CLI-only'.
- The install prompt is written as the USER's instruction to the agent, per skill, and is verifiable end-to-end: install first, probe the environment without leaking secrets, smoke-test with an HTTP status code ('It must return 200, not 401/429'), then 'Tell me how to restart or rescan my agent so it discovers the skill.' XERJ has install commands but no paste-into-agent setup prompt.
- Explicit handling of the non-interactive-shell trap in the prompt itself: 'Your shell is likely non-interactive and does NOT auto-source interactive profiles like ~/.zshrc or ~/.bashrc, so a key I set there can look present to me but empty to you.' This is the same class of gotcha as XERJ's PATH warning, but Exa gives the agent the diagnostic command and the recovery (`source ...;` prefix).
- Two-layer llms.txt: a curated root https://exa.ai/llms.txt whose first two sections are '## MCP Server' and '## Agent Skills', and an auto-generated docs index. The agent-native entry points are the first thing an agent sees, before products or pricing.
- Every docs page's .md twin carries a 3-line banner pointing back to llms.txt ('Fetch the complete documentation index at: https://exa.ai/docs/llms.txt / Use this file to discover all available pages before exploring further.'), so an agent landing on any deep page can find the index.
- Per-client MCP config-location table (9 rows on the docs page, 11 in the README) naming the file path AND the URL key name per client (`url` vs `serverUrl` vs `httpUrl`), plus the `mcp-remote` bridge for clients without remote MCP support and a 'Config file not found' troubleshooting accordion.
- The skill body SKILL.md frontmatter uses trigger phrasing ('Use when an agent needs ...' / 'including phrases like research this, find everything about') so the agent knows when to invoke it, and the MCP-shipped skill tells the agent what to do on failure: 'On auth / rate-limit errors, surface the fix (prefer OAuth) — don't fall back to generic web search.'
- Tone: makes no demands of the agent; nothing is 'required'. The only imperative text is the user-voiced setup prompt.

## What XERJ does better

- XERJ's llms.txt is a curated, agent-addressed document with a blockquote summary, 'Start here' with the real install command at line 11, a first-run ordered list, and an inline Caveats section. Exa's docs/llms.txt is a plain auto-generated Mintlify index with nothing about installation in the file itself; every install fact is one hop away.
- Honesty content: XERJ states its limits inline ('The DEFAULT embedding mode is lexical, not neural', 'fusion:"learned" is NOT implemented', RSS-runaway defect, measured kNN recall). Exa's agent files contain no caveats section and no claim qualifiers.
- XERJ has a feedback/contribution loop at all (field report, `xerj feedback --open-pr`, AI_CONTRIBUTIONS.md). Exa's agent-facing files have zero contribution or feedback ask; the only contact is support email.
- XERJ's llms-full.txt is curated (1,235 lines, 84 KB, numbered sections referenced as '§3', '§10' from llms.txt). Exa's llms-full.txt is a 767 KB raw-MDX concatenation that opens with Billing and contains unrendered JSX and radio-button markup; install content is buried at line ~6,700 of 19,818.
- XERJ's install is checksum-verified and documents a manual verified path; Exa has nothing comparable to document because it is a hosted API (not a fair comparison, but XERJ's is the more complete install spec).
- XERJ's 'Answers' tree gives question-shaped entries ('Can ChatGPT search a folder on my laptop?') that match how agents search; Exa's index entries are product-shaped.

## Adoptable ideas

- Add a per-operation COPYABLE SETUP PROMPT written in the user's voice, modelled on https://exa.ai/docs/get-started/agent-skills/build-with-exa.md 'Option B: Copy this prompt into your coding agent.' Structure to copy verbatim: 'Goal:' / 'Selected agent:' (with global vs project-local dirs per agent) / 'What to do:' numbered 1-5 ending with 'Smoke-test ... print only the status code ... It must return 200' and 'Tell me how to restart or rescan my agent so it discovers the skill.' For XERJ the smoke test is `curl -s -o /dev/null -w "%{http_code}\n" http://localhost:9200/_cluster/health`. This directly fixes the 'policy-constrained persona flipped to success once a documented manual path existed' finding by making the manual path a script the human hands the agent.
- Give the shell-less/MCP-only persona a route that needs no terminal, the way Exa leads with 'Exa offers a hosted server that works in any MCP client: https://mcp.exa.ai/mcp ... No API key is required to get started.' For XERJ: publish the MCP config JSON snippet and a `claude mcp add` / `codex mcp add` line in llms.txt itself (not behind `xerj init`), and copy Exa's 'Other clients' table format: Client | Where to add it | URL key (https://exa.ai/docs/get-started/exa-mcp.md).
- Add one-click install links in the README/landing like the exa-mcp-server README badges: `https://cursor.com/en/install-mcp?name=exa&config=<base64 json>` and `https://vscode.dev/redirect/mcp/install?name=exa&config=<urlencoded json>`. Both URL schemes are generic; XERJ can generate them for its local MCP server command.
- Steal the non-interactive-shell diagnostic wording for XERJ's PATH problem (llms.txt line 16). Exa's prompt: 'Your shell is likely non-interactive and does NOT auto-source interactive profiles like ~/.zshrc or ~/.bashrc, so a key I set there can look present to me but empty to you.' XERJ equivalent: tell the agent that `xerj` on PATH for the human may be missing for the agent, and give the presence test (`command -v xerj || ls -l $HOME/.local/bin/xerj`) plus the recovery prefix.
- Put a 3-line index banner at the top of every .md answer/doc twin, verbatim pattern from Exa pages: '> ## Documentation Index / > Fetch the complete documentation index at: https://exa.ai/docs/llms.txt / > Use this file to discover all available pages before exploring further.' XERJ's answers/*.md twins are the pages agents actually land on from search.
- Add 'Use when ...' trigger phrasing to the frontmatter description of the 10-line skill `xerj init` writes, copying the SKILL.md style: 'Use when an agent needs ... including phrases like ...' (https://raw.githubusercontent.com/exa-labs/exa-mcp-server/main/skills/search/SKILL.md). Also copy its failure instruction shape: 'On auth / rate-limit errors, surface the fix ... don't fall back to generic web search' -> for XERJ: 'On connection refused, start the node ... don't fall back to grep.'
- Fix the 'required'/'Optional' contradiction by following Exa's placement logic, not its (absent) ask: Exa places its agent-native calls to action in the FIRST sections of https://exa.ai/llms.txt ('## MCP Server', '## Agent Skills') and its Skills overview says 'Each skill page includes a one-line description, a copyable setup prompt, and a link to the raw SKILL.md source.' Inference (not from Exa, which has no feedback ask): rename XERJ's '## Optional' heading to something like '## Your side of the deal (the field report)' and place it directly after 'Start here', with the exact `xerj feedback --open-pr ...` command inline, so the required step is not filed under a heading that says it is not. Exa demonstrates no project in this category is asking agents for reports at all, so XERJ's ask is unusual and needs to be unmistakable.
- Mirror Exa's 'Option A / Option B' framing for install everywhere: 'Option A: Install this skill directly:' (one command) then 'Option B: Copy this prompt into your coding agent.' XERJ's Start-here currently gives only Option A.
