# Cursor

Category: editor / agent (closed source; no GitHub star count applies)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://cursor.com/llms.txt> — HTTP 200
- Size: 21678 bytes, 455 lines
- Install position: absent as text. The file is a bare URL index with no descriptions and no commands; the only install-related content is link lines: line 134 and line 182 `- https://cursor.com/docs/cli/installation.md`, line 325 `- https://cursor.com/help/getting-started/install.md`, line 441 `- https://cursor.com/help/troubleshooting/install-issues.md`. The only prose in the whole file is the final '## Internationalization' paragraph (lines 447-455). https://cursor.com/docs/llms.txt returns the identical file (200, 21678 bytes).
- Tone: neutral / inert. A machine-generated sitemap: no imperatives, no marketing, makes no demands of the agent. The only sentence addressed to the reader is the internationalization note ("To access a specific language version, prepend the language code to the path.").
- Section order: # Cursor Documentation → ## Get Started → ## Agent → ## grok-bot → ## customizing → ## cloud-agents → ## origin → ## Integrations → ## SDK → ## cli → ## Account → # CLI Documentation → ## Get Started → ## Headless → ## Reference → # API Documentation → ## API Overview → ## Cloud Agents API → ## Admin API → ## Analytics API → ## AI Code Tracking API → ## Origin API → # Help Center → ## Getting started → ## AI features → ## Customization → ## Models and usage → ## Security and privacy → ## Account and billing → ## Integrations → ## Grok Bot → ## Troubleshooting → ## Internationalization

### Install commands found

```sh
(none in llms.txt itself) — from the linked https://cursor.com/docs/cli/installation.md: `curl https://cursor.com/install -fsS | bash`
(linked page) `irm 'https://cursor.com/install?win32=true' | iex`
(linked page) `agent --version`
(linked page) `echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc` / `source ~/.bashrc`
(linked page) `agent`
(linked page) `agent update`
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

````text
```json title="CLI Server - Node.js"
{
  "mcpServers": {
    "server-name": {
      "command": "npx",
      "args": ["-y", "mcp-server"],
      "env": {
        "API_KEY": "value"
      }
    }
  }
}
```
(from https://cursor.com/docs/mcp.md, section '### Using `mcp.json`'; a 'Remote Server' variant with `"url": "http://localhost:3000/mcp"` and `"headers"` follows it)
````

### Feedback / contribution ask (verbatim)

```text
(none in llms.txt — only the link line `- https://cursor.com/help/troubleshooting/reporting-bugs.md`). That page says: "## Where do I report Cursor bugs?

Post on [forum.cursor.com](https://forum.cursor.com) for community help and visibility."
```

### Best passage for driving install and first use (verbatim)

````text
## Getting started

```bash
# Install (macOS, Linux, WSL)
curl https://cursor.com/install -fsS | bash

# Install (Windows PowerShell)
irm 'https://cursor.com/install?win32=true' | iex

# Run interactive session
agent
```

(from https://cursor.com/docs/cli/overview.md — NOT from llms.txt; llms.txt contains no such passage. The install page https://cursor.com/docs/cli/installation.md adds a '### Verification' step: `agent --version`, then a '## Post-installation setup' with the PATH export and `agent`.)
````

### One-click links

- `(none in llms.txt or in cursor.com/docs/mcp.md) — the deeplink format https://cursor.com/en/install-mcp?name=<name>&config=<base64 json> is used by third parties (e.g. Context7 README) but is NOT documented on Cursor's own MCP page; grep for `install-mcp` / `cursor://` in the fetched cursor.com markdown returned nothing`

## llms-full.txt

- URL: <https://cursor.com/llms-full.txt> — HTTP 200
- Size: 142881 bytes
- Relationship to llms.txt: Effectively does not exist. https://cursor.com/llms-full.txt answers 200 but with content-type text/html (142,881 bytes, 289 lines): it is the cursor.com landing page (<title>Cursor - The best way to code with AI</title>), i.e. a soft-404 from a catch-all SPA route. https://cursor.com/docs/llms-full.txt is a hard 404 (24,810-byte HTML error page). https://docs.cursor.com/llms.txt and /llms-full.txt both 200-redirect to https://cursor.com/docs (539 KB HTML). The same catch-all returns 200+HTML for /llms-install.md, /AGENTS.md, /SKILL.md, /CLAUDE.md, so none of those exist either. Cursor's 'full' content is instead served per-page as markdown: every docs URL has a `.md` twin (e.g. https://cursor.com/docs/mcp.md is text/markdown), and each `.md` page ends with a footer '## Sitemap\n\n[Overview of all docs pages](/llms.txt)'.
- Install position: absent — the response is not an llms-full file

## Other agent-facing files

### docs/mcp.md (MCP install page, markdown) — HTTP 200

<https://cursor.com/docs/mcp.md>

Full MCP guide: one-click marketplace install, mcp.json config (stdio/SSE/HTTP), static OAuth, config interpolation, enterprise allowlists, FAQ. 410 lines / 16,868 bytes. Does not document the install-mcp deeplink.

```text
### One-click installation

Browse the [Cursor Marketplace](/marketplace) for official plugins with one-click install from **Customize**, or configure custom servers with `mcp.json`. For community plugins and MCP servers, browse [cursor.directory](https://cursor.directory). Click "Add to Cursor" on a marketplace entry to install it and authenticate with OAuth.
```

### docs/cli/installation.md — HTTP 200

<https://cursor.com/docs/cli/installation.md>

CLI install page: one curl|bash line, one PowerShell line, a Verification step, PATH post-install, auto-update. 67 lines / 1,017 bytes.

````text
### Verification

After installation, verify that Cursor CLI is working correctly:

```bash
agent --version
```

## Post-installation setup

1. **Add \~/.local/bin to your PATH:**
````

### docs/cli/overview.md — HTTP 200

<https://cursor.com/docs/cli/overview.md>

CLI overview; opens with a 3-command 'Getting started' block (install mac/linux, install windows, run `agent`), then modes table.

```text
# Install (macOS, Linux, WSL)
curl https://cursor.com/install -fsS | bash

# Install (Windows PowerShell)
irm 'https://cursor.com/install?win32=true' | iex

# Run interactive session
agent
```

### help/troubleshooting/reporting-bugs.md — HTTP 200

<https://cursor.com/help/troubleshooting/reporting-bugs.md>

Bug-report guidance: checklist of what to include, where to post (forum), Request ID mechanics, privacy-mode caveats.

```text
## What should I include in a bug report?

- **Cursor version**: Click **Cursor** > **About Cursor** in the menu bar
- **Operating system**: Mac, Windows, or Linux, plus the version
- **Steps to reproduce**: What you did before the issue happened
- **Expected behavior**: What you expected to happen
- **Actual behavior**: What happened instead
```

### help/getting-started/install.md — HTTP 200

<https://cursor.com/help/getting-started/install.md>

Editor (GUI) install in six numbered steps; human-oriented (download button, drag to Applications).

```text
Get Cursor running on your machine in under five minutes.

## What do I need before installing?

A Cursor account. Sign up free at [cursor.com](https://cursor.com) if you don't have one.
```

### docs/get-started/quickstart.md — HTTP 200

<https://cursor.com/docs/get-started/quickstart.md>

Install-to-first-change walkthrough with apt/dnf repo setup for Linux.

```text
This guide gets you from install to your first useful change in Cursor. You'll sign in, ask Cursor to explain your codebase, make a small edit, and review the result.
```

### docs.md (docs root as markdown) — HTTP 200

<https://cursor.com/docs.md>

Landing page of docs in markdown, task-oriented headings ('Understand your code', 'Find and fix bugs').

```text
Cursor is a coding agent for building ambitious software. Use it to understand your codebase, plan and build features, fix bugs, review changes, and work with the tools you already use.
```

### llms-install.md / AGENTS.md / SKILL.md / CLAUDE.md — HTTP 200

<https://cursor.com/llms-install.md>

DO NOT EXIST — all four answer 200 with the same ~142 KB landing-page HTML (soft-404 catch-all route).

## What they do better than XERJ

- Every docs page has a `.md` twin at the same URL + `.md` (text/markdown), so an agent that lands anywhere in the tree can read clean markdown; the llms.txt index is nothing but those `.md` URLs. XERJ has this for /answers/*.md but not uniformly for /docs/*.
- Every markdown page ends with a fixed footer `## Sitemap\n\n[Overview of all docs pages](/llms.txt)` — a return path to the index from any leaf page.
- The install page is minimal and verifiable: install line, `agent --version` verification step, PATH post-install as a numbered step, `agent update`. 67 lines, no caveats, nothing to misread.
- Bug-report page is a plain checklist (version, OS, repro, expected, actual, request ID) with one destination (forum) — low friction, no obligation language.
- Internationalization block: one paragraph explains a URL-prefix convention (`/es/`, `/ja/`, `/cn/`) instead of listing 5x URLs.

## What XERJ does better

- XERJ's llms.txt is self-contained: install commands, the PATH consequence, the ordered first-run sequence, caveats and the field-report command are inline. Cursor's llms.txt is a bare 455-line URL list with zero commands, zero descriptions and zero install prose — an agent must fetch 2-3 more pages to learn how to install anything.
- XERJ describes every link (what the page is for and the gotcha it resolves); Cursor's index has no link descriptions at all.
- XERJ has a real llms-full.txt (1,235 lines / 84 KB). Cursor's /llms-full.txt is a soft-404 (200 + landing-page HTML) and /docs/llms-full.txt is a hard 404; the directory listings pointing at docs.cursor.com/llms*.txt are stale.
- XERJ documents the shell-less/HTTP-only path (Agent quickstart) and the MCP tool name; Cursor's index never mentions the no-shell case (though Cursor is an editor so it matters less).
- XERJ asks for feedback and gives a one-command path (`xerj feedback --open-pr`); Cursor asks for nothing from agents at all.
- XERJ states caveats up front (checksum fail-closed, data-dir-outside-index, exit codes). Cursor's install page has none — fine for Cursor's maturity, not for an rc.

## Adoptable ideas

- Sitemap footer on every agent-readable page. Cursor appends to every `.md` page: `---\n\n## Sitemap\n\n[Overview of all docs pages](/llms.txt)`. Add the same footer to every xerj.org /answers/*.md and /docs/*.md so an agent that arrives via search can always get back to https://xerj.org/llms.txt.
- `.md` twin for every docs URL (Cursor: https://cursor.com/docs/mcp.md is text/markdown while /docs/mcp is HTML). XERJ already does this for /answers/*.md; extend it to /docs/install.md, /docs/cli.md, /docs/security.md, /docs/agents/quickstart.md and list the .md URLs in llms.txt so an agent never has to scrape HTML.
- Explicit Verification step right after install, before anything else. Cursor's https://cursor.com/docs/cli/installation.md has `### Verification` → `agent --version`. XERJ's Start-here goes straight from installer to `xerj --insecure --data-dir`; insert `xerj --version` (with the full-path form `$HOME/.local/bin/xerj --version`) as step 0 so the PATH problem surfaces on a harmless command.
- Number the PATH fix as a post-install step with copy-pasteable lines per shell, as Cursor does: `echo 'export PATH="$HOME/.local/bin:$PATH"' >> ~/.bashrc` / `source ~/.bashrc` and the zsh pair. XERJ describes the consequence in prose; give the exact two lines.
- Make the feedback ask a checklist with one destination, not an obligation. Cursor's https://cursor.com/help/troubleshooting/reporting-bugs.md is `## What should I include in a bug report?` + 8 bullets + `## Where do I report Cursor bugs?`. Rename XERJ's `## Optional` heading to `## Your field report (required)` (the line-7 text says required and then points at 'Optional' — that contradiction is the bug) and restructure the entry as: what to include (5 bullets), the one command, the no-gh fallback.
- Cursor's whole MCP install story is two sentences plus a 10-line `mcp.json` block. XERJ's llms.txt mentions `xerj init` writes `.mcp.json` but never shows the resulting JSON; paste the exact `{"mcpServers":{"xerj":{...}}}` block verbatim so a shell-less/MCP-only persona can configure it by hand — this is the persona that fails at every model tier in the 22-run study.
- Do not publish deeplinks you don't document. Cursor's own mcp.md never mentions the `cursor.com/en/install-mcp?name=&config=` format that third parties use; if XERJ ships one-click links (`cursor://`, `claude mcp add ...`) put them in llms.txt itself, not only in the README.
- Language-prefix convention paragraph (Cursor: `prepend the language code to the path … Available languages: cn, ru, ja, pt-BR, es`) — only if/when xerj.org is translated; not a priority.
