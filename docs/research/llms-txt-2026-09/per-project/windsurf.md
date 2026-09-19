# Windsurf (formerly Codeium)

Category: editor / agent

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.windsurf.com/llms.txt> — HTTP 200
- Size: 24156 bytes, 132 lines
- Install position: Line 5, the very first entry under ## Docs: "- [Welcome to Windsurf](https://docs.windsurf.com/windsurf/getting-started.md): Download and install Windsurf IDE for Mac, Windows, or Linux. Import VS Code or Cursor settings, configure themes, and start coding with AI-powered assistance." Also line 66 for plugins: "- [Welcome to Windsurf Plugins](https://docs.windsurf.com/plugins/getting-started.md): Install and set up Windsurf Plugins for JetBrains, VS Code, Visual Studio, Vim, NeoVim, Jupyter, Chrome, and other IDEs with AI-powered coding assistance." No command anywhere in llms.txt — it is a link description only.
- Tone: neutral — a plain auto-generated Mintlify docs index (title + one-line description per page). It makes no demands of the agent at all; there is no preamble, no 'you' addressed to the reader, no instructions.
- Section order: # Windsurf Docs → ## Docs (125 link entries, flat: Windsurf IDE pages -> Cascade -> Accounts/API reference -> Troubleshooting -> Plugins -> Best practices) → ## Optional (Discord Community / Windsurf Blog / Support, repeated in ~8 languages)

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
- [Welcome to Windsurf](https://docs.windsurf.com/windsurf/getting-started.md): Download and install Windsurf IDE for Mac, Windows, or Linux. Import VS Code or Cursor settings, configure themes, and start coding with AI-powered assistance.
- [Recommended Extensions](https://docs.windsurf.com/windsurf/recommended-extensions.md): Popular Open VSX extensions for Windsurf including Python, Java, C#, GitLens, and more. Replicate familiar IDE experiences from VS Code, Eclipse, or Visual Studio.
- [Model Context Protocol (MCP)](https://docs.windsurf.com/windsurf/cascade/mcp.md): Integrate MCP servers with Cascade to access custom tools like GitHub, databases, and APIs. Configure stdio, HTTP, and SSE transports with admin controls for Teams.
```

## llms-full.txt

- URL: <https://docs.windsurf.com/llms-full.txt> — HTTP 200
- Size: 636246 bytes
- Relationship to llms.txt: Mintlify auto-concatenation of every page listed in llms.txt, each rendered as '# <title>\nSource: <url>\n<description>\n<page body with raw MDX tags: <Frame>, <Card>, <Tabs>, <Step>, inline SVG>'. Same content, not curated, page order is alphabetical-by-path (autocomplete/ first, windsurf/getting-started near the end), so the installation page is buried. 15,051 lines, 636 KB — far beyond any agent context window without search.
- Install position: Not first. The file opens with '# Autocomplete Overview' (line 1). The '# Welcome to Windsurf' install page is at line 13595 of 15051 (~90% of the way down). The install instructions there are GUI-only: "To get started, please ensure that your device meets the requirements, click the download link, and follow the instructions to install and run Windsurf." followed by minimum-OS lines (OS X Yosemite / Windows 10 / Ubuntu >= 20.04 or glibc >= 2.31) and an Onboarding section ("You can also optionally install `windsurf` in PATH such that you can run `windsurf` from your command line."). Plugins install (line ~4443-4790) is marketplace click-through per IDE; the only shell command in the whole file is the Jupyter one: "!{sys.executable} -m pip install -U codeium-jupyter --user". Feedback ask appears at line 5539-5540: "You can share feature requests and feedback through our community channels: [Reddit](https://www.reddit.com/r/windsurf/), [Discord](https://discord.com/invite/3XFf78nAx5), or [Twitter/X](https://x.com/windsurf)." MCP config snippet at line 3725+ (the ~/.codeium/mcp_config.json GitHub example, quoted below in other_files).

## Other agent-facing files

### docs.codeium.com/llms.txt and llms-full.txt — HTTP 200

<https://docs.codeium.com/llms.txt>

Legacy domain; 301-redirects to https://docs.windsurf.com/llms.txt and llms-full.txt (byte-identical: 24156 / 636246 bytes).

```text
# Windsurf Docs
```

### windsurf.com/llms.txt (marketing site root) — HTTP 200

<https://windsurf.com/llms.txt>

NOT an llms.txt — the Next.js site returns its HTML 404/landing page at that path (67,028 bytes, 1 line). Treat as absent.

```text
<!DOCTYPE html><html lang="en" class="scroll-smooth __variable_be8b38
```

### llms-install.md — HTTP 404

<https://docs.windsurf.com/llms-install.md>

does not exist

### AGENTS.md — HTTP 404

<https://docs.windsurf.com/AGENTS.md>

does not exist

### CLAUDE.md — HTTP 404

<https://docs.windsurf.com/CLAUDE.md>

does not exist

### SKILL.md — HTTP 404

<https://docs.windsurf.com/SKILL.md>

does not exist

### MCP install page (Cascade) — markdown variant — HTTP 200

<https://docs.devin.ai/desktop/cascade/mcp.md>

docs.windsurf.com/windsurf/cascade/mcp 301s to https://docs.devin.ai/desktop/cascade/mcp (HTML, 1.19 MB); the .md variant is 18,952 bytes. Documents GUI install via MCP Marketplace, a windsurf:// one-click deeplink, and raw ~/.codeium/mcp_config.json. Notably the .md page header tells the agent to fetch the docs index first.

````text
> ## Documentation Index
> Fetch the complete documentation index at: https://docs.devin.ai/llms.txt
> Use this file to discover all available pages before exploring further.

[...]

### One-Click Install via Deeplink

Devin Desktop supports one-click MCP installation through deeplinks. You can use these links to open the MCP
registry page directly in Devin Desktop, which is useful for sharing MCP server recommendations or embedding
install buttons in documentation.

The deeplink format is:

```
windsurf://windsurf-mcp-registry?serverName=<server-name>
```

[...]

```json theme={null}
{
  "mcpServers": {
    "github": {
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-github"
      ],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "<YOUR_PERSONAL_ACCESS_TOKEN>"
      }
    }
  }
}
```
````

### docs.devin.ai/llms.txt (successor docs, Cognition) — HTTP 200

<https://docs.devin.ai/llms.txt>

361 lines / 37,709 bytes. Same Mintlify format but with a nested hierarchy (## Cloud / ### Get Started / ### Essential Guidelines / ### Onboarding Devin / ... / ## CLI / ### Getting Started / ### Extensibility / #### MCP). Shows the Windsurf docs are being folded into Devin; still no install commands or agent prompt in the index itself.

```text
- [Devin MCP](https://docs.devin.ai/work-with-devin/devin-mcp.md): Set up the official Devin MCP server so external AI tools can manage sessions, playbooks, knowledge, and repository docs.
```

## What they do better than XERJ

- Every linked page has a per-page .md URL (https://docs.windsurf.com/<path>.md) so an agent can fetch exactly one page as clean markdown; XERJ mixes .md answers with HTML docs pages (e.g. https://xerj.org/docs/install has no .md variant in the index).
- Installation is the FIRST entry of the index (line 5) and its description states platforms in the first sentence: "Download and install Windsurf IDE for Mac, Windows, or Linux." XERJ's install commands are also near the top, but wrapped in a 3-paragraph preamble of demands before the first command.
- Each per-page .md (Devin successor docs) opens with a 3-line self-locating header: "> ## Documentation Index / Fetch the complete documentation index at: https://docs.devin.ai/llms.txt / Use this file to discover all available pages before exploring further." — any deep-linked page points the agent back to the index. XERJ's answer .md files do not.
- The MCP config is given as a complete, copy-pasteable ~/.codeium/mcp_config.json with stdio AND remote-HTTP AND env-interpolation variants, plus a one-click deeplink (windsurf://windsurf-mcp-registry?serverName=...). XERJ's llms.txt mentions `xerj init` writes .mcp.json but shows no raw snippet for a shell-less MCP-only persona.
- Consistent 1-sentence-per-page descriptions that read as searchable summaries (feature + what it does + who it's for); XERJ's entries are longer and carry warnings inline, which dilutes scannability.

## What XERJ does better

- Windsurf's llms.txt is a plain auto-generated Mintlify docs index with nothing about installation beyond a link description: zero install commands, zero agent-directed text, zero MCP snippet, zero feedback ask. XERJ ships actual commands (`curl -fsSL https://xerj.org/get | sh`, `irm https://xerj.org/get.ps1 | iex`) and an ordered first-run sequence inline.
- XERJ has a Caveats section inline ("the default embedding mode is lexical, not neural"); Windsurf's only honesty line is buried at llms-full line ~13660: "In some cases Windsurf may not function exactly as referenced, or documentation may be out of date."
- XERJ documents the shell-less path explicitly ([Agent quickstart] "the HTTP-only path for an agent with no shell") and the PATH-not-edited failure mode; Windsurf assumes a GUI human clicking a download button — an agent literally cannot install it from the docs.
- XERJ's llms-full.txt (84 KB) fits in a context window; Windsurf's 636 KB with raw MDX/SVG noise (inline SVG path data inside the Welcome page) does not.
- XERJ has a feedback mechanism with a command (`xerj feedback --open-pr ...`); Windsurf's only ask is the passive "[Let us know](https://discord.com/invite/3XFf78nAx5) if there are other common workflows you want wrapped in a slash command." and "You can share feature requests and feedback through our community channels: Reddit, Discord, or Twitter/X."

## Adoptable ideas

- Self-locating header on every per-page .md (verbatim from https://docs.devin.ai/desktop/cascade/mcp.md lines 1-3): "> ## Documentation Index\n> Fetch the complete documentation index at: https://docs.devin.ai/llms.txt\n> Use this file to discover all available pages before exploring further." — prepend the XERJ equivalent ("Fetch the complete documentation index at: https://xerj.org/llms.txt") to every https://xerj.org/answers/*.md and docs .md so an agent that lands on a deep page via search still finds the hero prompt and the field-report ask.
- Put the install page FIRST and describe it in platform terms in the first clause, as Windsurf does at line 5: "Download and install Windsurf IDE for Mac, Windows, or Linux." For XERJ: make the first line after the H1 the two install commands, and move the two-paragraph 'this file asks two things of you' preamble below them.
- Ship a raw, complete MCP config snippet for the shell-less persona, mirroring Windsurf's mcp_config.json block (https://docs.windsurf.com/plugins/cascade/mcp.md, '## mcp_config.json'): a `{"mcpServers":{"xerj":{"command":"xerj","args":["mcp"]}}}`-style stdio example AND a remote-HTTP example using their exact pattern `"serverUrl": "<your-server-url>/mcp", "headers": {"API_KEY": "Bearer ${env:AUTH_TOKEN}"}` — this is what the 22-run study said MCP-only personas were missing.
- Offer a one-click MCP install link per editor, as Windsurf documents: `windsurf://windsurf-mcp-registry?serverName=<server-name>` (from docs.devin.ai/desktop/cascade/mcp.md '### One-Click Install via Deeplink'). XERJ can list `cursor://anysphere.cursor-deeplink/mcp/install?name=xerj&config=<base64>` and `claude mcp add xerj -- xerj mcp` under Start here; Windsurf's phrasing to copy: "useful for sharing MCP server recommendations or embedding install buttons in documentation."
- Give every docs page a `.md` twin URL and link the .md in llms.txt (Windsurf: every one of 125 entries ends in `.md`). XERJ's [Install docs](https://xerj.org/docs/install), [CLI reference], [Security], [Agent quickstart] currently point at HTML.
- Rename XERJ's '## Optional' section. Windsurf uses '## Optional' correctly per the llms.txt spec — only Discord/Blog/Support live there — which is exactly why filing a 'required, not optional' field report under a heading literally named 'Optional' reads as a contradiction to an agent. Move the field-report template link into '## Start here' (next to step 4) or a new '## Your side of the deal (required)' section, and keep '## Optional' for community links only.
- Keep the honesty disclaimer but put it at the top, not at llms-full line 13660 like Windsurf's: "In some cases Windsurf may not function exactly as referenced, or documentation may be out of date." XERJ already does this with Caveats — retain it; do not copy Windsurf's burial.
- Anti-pattern to avoid (INFERRED from measurement, not stated by them): Windsurf's llms-full.txt is 636 KB of alphabetical page concatenation with raw MDX and inline SVG path data, install page at 90% depth. Keep XERJ's llms-full curated and ordered (install → first use → reference), never auto-concatenated.
