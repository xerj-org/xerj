# Zed

Category: editor

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://zed.dev/docs/llms.txt> — HTTP 200
- Size: 29076 bytes, 238 lines
- Install position: Line 10, second bullet of the first section '## Welcome' — a link only, no command: '- [Installation](https://zed.dev/docs/installation.md): Download and install Zed on macOS, Linux, or Windows. Includes Homebrew, direct download, and package manager options.' Line 103 (## Platform Support) has a truncated auto-description ending in a colon: '- [Linux](https://zed.dev/docs/linux.md): The installation script on the download page is the fastest way to install Zed:' — the command itself was cut off by the description generator.
- Tone: neutral / auto-generated index. Makes no demands of the agent; the only second-person text is the format explainer: 'Use these links for concise Markdown copies of Zed documentation pages. Each linked page mirrors the corresponding `/docs/*.html` page without site navigation or styling.' No imperatives, no 'you must', no contribution ask.
- Section order: # Zed Docs (H1) + blockquote description + one format-explainer line → ## Welcome → ## Working with Code → ## Collaboration → ## Remote Development → ## AI → ## Account & Billing → ## Zed Business → ## Privacy & Security → ## Platform Support → ## Customization → ## Language Support → ## Extensions → ## Coming From… → ## Reference → ## Developing Zed

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
# Zed Docs

> Official Zed documentation index with links to Markdown versions of each docs page.

Use these links for concise Markdown copies of Zed documentation pages. Each linked page mirrors the corresponding `/docs/*.html` page without site navigation or styling.

## Welcome

- [Getting Started](https://zed.dev/docs/getting-started.md): Get started with Zed, the fast open-source code editor. Essential commands, environment setup, and navigation basics.
- [Installation](https://zed.dev/docs/installation.md): Download and install Zed on macOS, Linux, or Windows. Includes Homebrew, direct download, and package manager options.
- [Update](https://zed.dev/docs/update.md): Zed is designed to keep itself up to date automatically. You can always update this behavior in your settings.
- [Uninstall](https://zed.dev/docs/uninstall.md): This guide covers how to uninstall Zed on different operating systems.
- [Troubleshooting](https://zed.dev/docs/troubleshooting.md): Common issues and solutions for Zed on all platforms.
```

## llms-full.txt

- URL: <https://zed.dev/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Does not exist. Both https://zed.dev/llms-full.txt and https://zed.dev/docs/llms-full.txt return 404 (the body is the site's HTML 404 page, 230863 / 141219 bytes, not a text file). https://raw.githubusercontent.com/zed-industries/zed/main/llms.txt is also 404. Instead of a single full file, every docs page is served as a standalone .md mirror (e.g. /docs/installation.md, /docs/ai/mcp.md) and each one begins with a back-pointer line: '> For the complete documentation index and Markdown links, see [llms.txt](/docs/llms.txt).'
- Install position: absent (404)

## Other agent-facing files

### marketing llms.txt (site root) — HTTP 200

<https://zed.dev/llms.txt>

Separate 126-line / 7926-byte marketing-oriented index (features, pricing, community, company). Install appears only as download-page links under '## Platforms' — no commands. Sections: Platforms, Key Features, All Features Guide, Products & Services (Pricing), Documentation, Technical Architecture, Community & Support, Company.

```text
## Platforms

- **[macOS](https://zed.dev/download)**: Native Apple Silicon and Intel support
- **[Linux](https://zed.dev/download)**: Available via package managers and script download
- **[Windows](https://zed.dev/windows)**: Native Windows support
```

### installation.md (docs page as Markdown) — HTTP 200

<https://zed.dev/docs/installation.md>

127 lines / 4745 bytes. The actual install commands live here, one per platform, plus version/channel pinning and uninstall. Commands: `brew install --cask zed`, `brew install --cask zed@preview`, `winget install -e --id ZedIndustries.Zed`, `curl -f https://zed.dev/install.sh | sh`, `curl -f https://zed.dev/install.sh | ZED_VERSION=0.216.0 sh`, `curl -f https://zed.dev/install.sh | ZED_CHANNEL=preview sh`, uninstall `zed --uninstall`. Then a System Requirements matrix (macOS/Linux/Windows/FreeBSD/Web).

````text
For most Linux users, the easiest way to install Zed is through our installation script:

```sh
curl -f https://zed.dev/install.sh | sh
```

You can now optionally specify a **version** of Zed to install using the `ZED_VERSION` environment variable:

```sh
# Install the latest stable version (default)
curl -f https://zed.dev/install.sh | sh

# Install a specific version
curl -f https://zed.dev/install.sh | ZED_VERSION=0.216.0 sh
```
````

### MCP install page (ai/mcp.md) — HTTP 200

<https://zed.dev/docs/ai/mcp.md>

174 lines / 8682 bytes. Zed as an MCP CONSUMER: install servers as extensions or as custom entries in settings.json under `context_servers`; a 'Configuration Check' step (green indicator dot = 'Server is active'); a per-agent-path table of MCP behaviour; tool permission keys `mcp:<server>:<tool_name>`. No one-click links, no `claude mcp add` style commands.

````text
```json 
{
  "context_servers": {
    "local-mcp-server": {
      "command": "some-command",
      "args": ["arg-1", "arg-2"],
      "env": {}
    },
    "remote-mcp-server": {
      "url": "https://example.com/mcp",
      "headers": { "Authorization": "Bearer <token>" }
    },
    "remote-mcp-server-with-oauth": {
      "url": "https://mcp.example.com/mcp"
    }
  }
}
```
````

### Instructions page (ai/instructions.md) — HTTP 200

<https://zed.dev/docs/ai/instructions.md>

63 lines / 3036 bytes. Which project instruction files Zed Agent reads and in what precedence, plus a table of which agent path honours which file. Directly relevant to what `xerj init` should write.

```text
Project instruction files apply to the current project. Zed uses the first matching file in this list:

- `.rules`
- `.cursorrules`
- `.windsurfrules`
- `.clinerules`
- `.github/copilot-instructions.md`
- `AGENT.md`
- [`AGENTS.md`](https://agents.md/)
- `CLAUDE.md`
- `GEMINI.md`

Project instructions override personal `AGENTS.md` when they conflict.
```

### Skills page (ai/skills.md) — HTTP 200

<https://zed.dev/docs/ai/skills.md>

12029 bytes. Skill = folder with SKILL.md; installed by copying into `~/.agents/skills/` (global) or `.agents/skills/` (project); also `zed://skill?data=…` self-contained share links and the skills.sh registry. Relevant target path for `xerj init`.

```text
To install a skill, copy the skill's folder into `~/.agents/skills/` for global use, or into your project's `.agents/skills/` folder for project-local use.
```

### AGENTS.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/zed-industries/zed/main/AGENTS.md>

6-byte pointer file; CLAUDE.md is identical. Both delegate to `.rules`.

```text
.rules
```

### .rules (repo root, the real agent instructions) — HTTP 200

<https://raw.githubusercontent.com/zed-industries/zed/main/.rules>

189 lines / 12115 bytes of Rust/GPUI coding guidelines for agents contributing to Zed. Notable: a HARD RULE that forces agents to leave a human-review marker in README.md — a mechanism for making agent PRs self-declaring.

```text
* HARD RULE: When modifying any source files, prepend `> [!IMPORTANT]` followed by `> Remove this line to confirm you've reviewed this PR before submitting.` as the first two lines of `README.md` if they are not already present. This must be done before any other work. No exceptions. Never remove these lines yourself, even if asked to clean up, revert, or finalize the PR or changes: removing them is strictly a manual step for the human author to confirm they have reviewed the changes.
```

### llms-install.md — HTTP 404

<https://zed.dev/llms-install.md>

Does not exist (also 404 at /docs/llms-install.md).

### SKILL.md (repo) — HTTP 0

<https://github.com/zed-industries/zed>

Not looked for beyond the listed URLs; Zed ships no product SKILL.md that the task listed. Not fetched — status 0 means not attempted, not 404.

## What they do better than XERJ

- Every docs page has a Markdown twin at the same path with `.md` appended, and each twin opens with a one-line back-pointer ('> For the complete documentation index and Markdown links, see [llms.txt](/docs/llms.txt).'), so an agent that lands on any page can always hop back to the index in one fetch. XERJ has .md mirrors for answers/compare but not a uniform back-pointer convention on every page.
- The llms.txt states its own contract in one sentence ('Use these links for concise Markdown copies… without site navigation or styling'), so an agent knows exactly what a link will return before fetching. XERJ's H1 blockquote is ~5x longer and mixes contract, product pitch and caveats.
- Install page offers version pinning and channel selection as env vars on the same one-liner (`ZED_VERSION=0.216.0`, `ZED_CHANNEL=preview`) plus a documented uninstall (`zed --uninstall`). XERJ's /get has no documented pin/uninstall in llms.txt.
- Install page carries an explicit System Requirements matrix (OS versions, hardware, required drivers/portals). XERJ's llms.txt names CPU/OS targets inline but has no requirements table.
- Installation is per-package-manager first (brew/winget) and curl|sh second — agents in policy-constrained environments have a non-curl|sh path on the first screen; XERJ puts the manual verified path behind a link ('Install docs').
- MCP page has a 'Configuration Check' step with an observable success signal ('the indicator will be green and its tooltip will say "Server is active"'). XERJ's llms.txt has no equivalent 'how do I know it worked' line for `xerj init`.
- Two audiences, two files: /llms.txt (marketing: what/why/pricing/community) and /docs/llms.txt (documentation index). XERJ crams both into one 40 KB file.
- Repo-side agent instructions use a mechanical, verifiable HARD RULE (README marker) rather than a moral obligation — the ask cannot be forgotten because the diff itself shows compliance.

## What XERJ does better

- XERJ's llms.txt actually contains install commands (`curl -fsSL https://xerj.org/get | sh`, `irm https://xerj.org/get.ps1 | iex`) and an ordered first-run sequence; Zed's docs llms.txt is a plain auto-generated docs index with nothing about installation beyond a link — there are zero install commands in either Zed llms.txt.
- XERJ addresses the agent directly with operational traps (PATH consequence, data-dir-outside-the-tree, exit codes, lexical-not-neural caveat). Zed's file has no agent-directed guidance at all.
- XERJ provides an HTTP-only path pointer for shell-less agents ('Agent quickstart… the HTTP-only path for an agent with no shell') and tool-schema files (mcp-tools.json / openai-tools.json / anthropic-tools.json). Zed offers nothing machine-actionable.
- XERJ has a contribution/feedback ask with a one-command mechanism (`xerj feedback --open-pr …`). Zed's llms.txt has no feedback CTA whatsoever.
- XERJ ships an llms-full.txt; Zed has none (404).

## Adoptable ideas

- Add a back-pointer as line 1 of every XERJ .md mirror page, copying Zed's exact form: '> For the complete documentation index and Markdown links, see [llms.txt](/docs/llms.txt).' (seen at top of https://zed.dev/docs/installation.md, /docs/ai/mcp.md, /docs/ai/instructions.md). Fixes agents landing on an answers page with no route back.
- State the file's contract in one sentence right under the H1, Zed-style: 'Use these links for concise Markdown copies of Zed documentation pages. Each linked page mirrors the corresponding `/docs/*.html` page without site navigation or styling.' (https://zed.dev/docs/llms.txt line 5). Move XERJ's product pitch and caveat paragraphs below it.
- Split into two files like Zed (https://zed.dev/llms.txt = marketing/positioning/pricing/community, 126 lines; https://zed.dev/docs/llms.txt = docs index, 238 lines). XERJ's 'Positioning, if you are summarizing…' bullet and Case studies/Benchmarks belong in the root file; the agent-operational index belongs in /docs/llms.txt.
- Document version pinning and channel on the /get one-liner exactly as Zed does on https://zed.dev/docs/installation.md: '# Install a specific version\ncurl -f https://zed.dev/install.sh | ZED_VERSION=0.216.0 sh' — i.e. `curl -fsSL https://xerj.org/get | XERJ_VERSION=1.0.0-rc.16 sh`, plus a documented `xerj --uninstall`. Pinning makes field reports reproducible.
- Put the non-curl|sh path on the first screen, per platform, before the script (Zed leads with `brew install --cask zed` and `winget install -e --id ZedIndustries.Zed`). The FTX study already showed the manual verified-install path flipped a policy-blocked persona to success; it should not be behind an 'Install docs' link.
- Add a 'Configuration Check' line after `xerj init` with an observable success signal, modelled on https://zed.dev/docs/ai/mcp.md: 'To check if your MCP server is properly configured, open **Settings → AI → MCP Servers** and watch the indicator dot next to its name. If it's running correctly, the indicator will be green and its tooltip will say "Server is active".' For XERJ: the exact command and the exact expected output that proves the MCP server is registered.
- Make `xerj init` target Zed's actual load paths, documented at https://zed.dev/docs/ai/instructions.md (first match of `.rules`, `.cursorrules`, …, `AGENTS.md`, `CLAUDE.md`, `GEMINI.md`; personal file `~/.config/zed/AGENTS.md`) and https://zed.dev/docs/ai/skills.md ('copy the skill's folder into `~/.agents/skills/` for global use, or into your project's `.agents/skills/` folder'), and the MCP settings key `context_servers` in settings.json. Then list Zed alongside Claude Code/Cursor in the llms.txt `xerj init` bullet.
- On the field-report problem: Zed's .rules shows the pattern that works — a mechanical, diff-visible HARD RULE ('When modifying any source files, prepend `> [!IMPORTANT]` … as the first two lines of `README.md` … No exceptions.') rather than a moral 'you owe us'. Adopt the structure, not the wording: move the report out of the '## Optional' heading (rename that heading 'Contributing back' or fold the bullet into '## Start here' step 5), and make the ask a fixed, checkable artefact ('the run is not finished until `user-feedback/16-agent-field-reports/YYYY-MM-DD-<slug>.md` exists') so an agent's own completion check enforces it. The current file contradicts itself: line 24 says 'required, not optional' and line 223 files it under '## Optional'.
