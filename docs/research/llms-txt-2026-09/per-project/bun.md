# Bun

Category: runtime / CLI

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://bun.com/llms.txt> — HTTP 200
- Size: 33721 bytes, 328 lines
- Install position: line 6, the 2nd link in '## Docs' (right after 'Welcome to Bun'). It is a link only, no command in llms.txt itself: '- [Installation](https://bun.com/docs/installation.md): Install Bun with npm, Homebrew, Docker, or the official script.'
- Tone: neutral / catalogue. It is a Mintlify auto-generated docs index: one H1, one '## Docs' heading with ~320 '- [Title](url.md): description' lines, and a 2-link '## Optional'. It makes no demands of the agent, gives no instructions, no prompt, no commands. The only mildly marketing line is the description of the Welcome page ('all-in-one toolkit'). The feedback link is the LAST line of '## Docs' (line 323), directly above '## Optional'.
- Section order: # Bun → ## Docs (lines 3-323, ~320 flat link lines; sub-groupings are only implicit by URL path: /docs/, /docs/runtime/, /docs/pm/, /docs/bundler/, /docs/test/, /docs/guides/*) → ## Optional (lines 325-328: Reference, Blog)

### Install commands found

```sh
(none in llms.txt itself; the linked https://bun.com/docs/installation.md (200) contains:)
curl -fsSL https://bun.com/install | bash
powershell -c "irm bun.sh/install.ps1|iex"
npm install -g bun # the last `npm` command you'll ever need
brew install oven-sh/bun/bun
scoop install bun
docker pull oven/bun
docker run --rm --init --ulimit memlock=-1:-1 oven/bun
bun --version
bun --revision
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

```text
- [Feedback](https://bun.com/docs/feedback.md): Share feedback, bug reports, and feature requests
```

### Best passage for driving install and first use (verbatim)

```text
# Bun

## Docs

- [Welcome to Bun](https://bun.com/docs/index.md): Bun is an all-in-one toolkit for developing modern JavaScript/TypeScript applications.
- [Installation](https://bun.com/docs/installation.md): Install Bun with npm, Homebrew, Docker, or the official script.
- [Quickstart](https://bun.com/docs/quickstart.md): Build your first app with Bun
- [TypeScript](https://bun.com/docs/typescript.md): Using TypeScript with Bun, including type definitions and compiler options

(from the linked installation.md, which is what actually drives install:)

Bun ships as a single, dependency-free executable. Install it with the install script, a package manager, or Docker on macOS, Linux, and Windows.

<Tip>After installation, verify with `bun --version` and `bun --revision`.</Tip>

      curl -fsSL https://bun.com/install | bash

<Warning>
  If you've installed Bun but are seeing a `command not found` error, you may have to manually add the installation
  directory (`~/.bun/bin`) to your `PATH`.
</Warning>
```

## llms-full.txt

- URL: <https://bun.com/llms-full.txt> — HTTP 200
- Size: 2136096 bytes
- Relationship to llms.txt: Mintlify-generated concatenation of every docs page's Markdown (each page = '# Title' + 'Source: <url>' + description + body, with Mintlify JSX components like <Tabs>, <Tip>, <Warning> and theme={...} attributes left in). Same content as the individual .md pages linked from llms.txt; nothing curated, nothing agent-specific, no install prompt. Not in llms.txt order. Only agent-related content anywhere in it is the `bun init` page (line ~47750): 'AI Agent rules (disable with `$BUN_AGENT_RULE_DISABLED=1`): * a `CLAUDE.md` file when `bun init` detects Claude CLI (disable with `CLAUDE_CODE_AGENT_RULE_DISABLED` env var) * a `.cursor/rules/*.mdc` file when `bun init` detects Cursor (disable with `CURSOR_AGENT_RULE_DISABLED` env var); the file tells [Cursor AI](https://cursor.sh) to use Bun instead of Node.js and npm'. No 'mcp', 'claude mcp add', 'cursor://' or 'vscode:' strings occur anywhere in the 2.1 MB file.
- Install position: The '# Installation' page begins at line 21421 of 56073 (38% of the way through the file). The file does NOT start with Welcome/Installation; line 1 is '# Bytecode Caching\nSource: https://bun.com/docs/bundler/bytecode' -- pages appear to be concatenated in an order unrelated to the llms.txt order (alphabetical-by-path or build order). Install command 'curl -fsSL https://bun.com/install | bash' appears at lines 21438 and 28126; a pinned-version form 'curl -fsSL https://bun.sh/install | bash -s "bun-v1.3.3"' at 20738/21625.

## Other agent-facing files

### bun.sh/llms.txt + bun.sh/llms-full.txt — HTTP 200

<https://bun.sh/llms.txt>

Same bytes as bun.com (per task brief; not re-verified here). Alternate domain.

```text
# Bun

## Docs
```

### CLAUDE.md (repo root, oven-sh/bun) — HTTP 200

<https://raw.githubusercontent.com/oven-sh/bun/main/CLAUDE.md>

17,609 bytes / 240 lines. Contributor-agent guide for working ON the Bun codebase (build, test, architecture, PR review rules) -- NOT for users installing Bun. Sections: Building and Running Bun / Testing / Code Architecture / JavaScript Modules / Landing PRs: What Bun Reviewers Catch / Important Development Notes / Debugging CI Failures / Reading PR Feedback. Notable: it points to a separate REVIEW.md 'distilled from ~2,500 merged PRs' and to `.claude/docs/landing-prs.md` situational sections; it makes hard imperative demands with CRITICAL markers.

```text
## Important Development Notes

1. **Never use `bun test` or `bun <file>` directly** - always use `bun bd test` or `bun bd <command>`. `bun bd` compiles & runs the debug build.
2. **All changes must be tested** - if you're not testing your changes, you're not done.
3. **Get your tests to pass**. If you didn't run the tests, your code does not work.

(and, from 'Landing PRs':) The code review rules — what blocks merges, distilled from ~2,500 merged PRs — live in `REVIEW.md`. Read it before writing code that makes a non-obvious choice.
```

### AGENTS.md (repo root) — HTTP 200

<https://raw.githubusercontent.com/oven-sh/bun/main/AGENTS.md>

9-byte pointer file whose entire content is the filename of CLAUDE.md; a cross-tool alias so Codex/other agents find the same guide.

```text
CLAUDE.md
```

### installation.md (docs page linked 2nd in llms.txt) — HTTP 200

<https://bun.com/docs/installation.md>

9,107 bytes / 341 lines. The real install page: script / PowerShell / npm / brew / scoop / Docker tabs, verify step, PATH-fix accordion, upgrade, canary, uninstall. Contains the exact 'command not found' recovery XERJ also documents.

```text
To check that Bun was installed successfully, open a new terminal window and run:

bun --version
# Output: 1.x.y

# See the precise commit of `oven-sh/bun` that you're using
bun --revision
# Output: 1.x.y+b7982ac13189

<Warning>
  If you've installed Bun but are seeing a `command not found` error, you may have to manually add the installation
  directory (`~/.bun/bin`) to your `PATH`.
</Warning>
```

### feedback.md (last link in llms.txt Docs section) — HTTP 200

<https://bun.com/docs/feedback.md>

Human-oriented 3-step issue-filing guide (upgrade first, search existing, then report with template links). Not agent-specific, no obligation language.

```text
Here's how to open a helpful issue for a bug, a performance problem, or a feature request:

<Step title="Upgrade Bun">
	Upgrade Bun to the latest version with `bun upgrade`. This might fix your problem without opening an issue.
...
- 🐞 [**Report a Bug**](https://github.com/oven-sh/bun/issues/new?template=2-bug-report.yml)
- ⚡ [**Suggest an Improvement**](https://github.com/oven-sh/bun/issues/new?template=4-feature-request.yml)

Provide as much detail as possible, including:
- A clear and concise title
- A code example or steps to reproduce the issue
- The version of Bun you are using (run `bun --version`)
```

### llms-install.md — HTTP 404

<https://bun.com/llms-install.md>

Does not exist (also 404 at https://bun.com/docs/llms-install.md).

### SKILL.md — HTTP 404

<https://raw.githubusercontent.com/oven-sh/bun/main/SKILL.md>

Does not exist at repo root.

### MCP install page — HTTP 404

<https://bun.com/docs/mcp.md>

Does not exist. Bun ships no MCP server and its llms files never mention MCP.

## What they do better than XERJ

- Small, scannable llms.txt: 328 lines / 33.7 KB, one flat catalogue with a 1-sentence description per link. An agent finds Installation on line 6 in the first screen; XERJ's 40 KB file front-loads ~30 lines of dense prose (installer checksum tool fallback list, PATH warning, data-dir warning) before the numbered first-run list.
- Install page has an explicit verify step with expected output ('bun --version  # Output: 1.x.y' and 'bun --revision') and a clearly boxed <Warning> for 'command not found' with the fix. XERJ has the PATH warning but no 'expected output' line an agent can pattern-match to confirm success.
- Install-time agent onboarding is built into the product, not the doc: `bun init` writes a CLAUDE.md and `.cursor/rules/*.mdc` automatically when it detects Claude CLI / Cursor, with opt-out env vars. XERJ's `xerj init` is similar but is marked '(optional, one command)' and buried as step 3; Bun makes it a side effect of the normal first command.
- Multiple install channels listed in one breath (script, PowerShell, npm, brew, scoop, Docker incl. distroless/alpine variants) -- a shell-less or policy-constrained agent has a package-manager or container route it can name to its operator. XERJ lists only the curl/irm script (plus a manual verified path elsewhere).
- CLAUDE.md for contributors is separated from user docs and is ruthlessly imperative with CRITICAL markers and a REVIEW.md 'distilled from ~2,500 merged PRs' -- contribution rules live where a contributing agent reads them, not in the user-facing llms.txt.
- AGENTS.md as a 9-byte pointer to CLAUDE.md: zero-maintenance cross-tool discovery (Codex, Cursor, Claude all land on one file).

## What XERJ does better

- XERJ's llms.txt actually addresses the agent: install commands are inline (line 11-12), a numbered first-run sequence, caveats, and a 'Start here' section. Bun's llms.txt contains zero commands and zero instructions -- it is a plain auto-generated Mintlify docs index; an agent must follow a link to install anything.
- XERJ has a feedback/contribution ask aimed at agents (`xerj feedback --open-pr ...`, dry-run fallback, 'hand the printed report to your operator'). Bun's only feedback surface is a human GitHub-issue guide with no agent framing and no ask.
- XERJ documents MCP wiring (`xerj init` registers `.mcp.json`) and a skill; Bun has no MCP anywhere (404 page, zero mentions in 2.1 MB llms-full).
- XERJ's llms-full.txt (84 KB) is a size an agent can actually read in one context; Bun's 2.1 MB / 56k-line file is unreadable in one pass, is not in llms.txt order, starts at 'Bytecode Caching', and leaves Mintlify JSX (<Tabs>, theme={...}) in the text.
- XERJ has a Caveats section stating what NOT to rely on (honest-claims); Bun's index has nothing equivalent.
- XERJ states the PATH consequence up front in llms.txt itself; Bun only says it on the install page.

## Adoptable ideas

- Verify-step with literal expected output, copied from https://bun.com/docs/installation.md: 'To check that Bun was installed successfully, open a new terminal window and run: bun --version  # Output: 1.x.y'. XERJ: add after the installer lines 'xerj --version  # Output: xerj 1.0.0-rc.NN' so an agent (and the field-report auto-fill) has an unambiguous success check.
- Description-per-link catalogue discipline from llms.txt line 6: '- [Installation](https://bun.com/docs/installation.md): Install Bun with npm, Homebrew, Docker, or the official script.' -- every XERJ link line should name the install channels/outcome in its one-sentence description, and the 'Start here' block should shrink to install + verify + first run, with checksum/tool-fallback prose moved to the linked page.
- Make agent-rule generation a side effect of the normal first command, as `bun init` does (llms-full line ~47750): 'AI Agent rules (disable with `$BUN_AGENT_RULE_DISABLED=1`): * a `CLAUDE.md` file when `bun init` detects Claude CLI ... * a `.cursor/rules/*.mdc` file when `bun init` detects Cursor'. XERJ: have `xerj autoindex` (step 2) detect Claude Code/Cursor and offer/perform the `xerj init` wiring, with an opt-out env var, instead of listing it as '(optional)' step 3 -- this directly addresses the MCP-only persona failure.
- Add package-manager / container install channels to the Start-here block, named the way Bun does: 'npm install -g bun', 'brew install oven-sh/bun/bun', 'scoop install bun', 'docker pull oven/bun'. For XERJ: a `brew tap`, a `docker pull ghcr.io/xerj-org/xerj`, and a manual 'download + sha256 verify' path listed side by side give shell-less/policy-constrained agents a route they can hand to an operator.
- Ship a 9-byte AGENTS.md pointer exactly as https://raw.githubusercontent.com/oven-sh/bun/main/AGENTS.md does (content: 'CLAUDE.md') so all agent tools resolve to one repo guide; conversely keep contributor rules (AI_CONTRIBUTIONS.md, field-report protocol) out of the user-facing llms.txt tail and in the file agents read when contributing.
- Fix the 'required but under Optional' contradiction using Bun's structure as the reference: Bun reserves '## Optional' strictly for skippable extras (Reference, Blog) and puts Feedback as the LAST line of the main '## Docs' section. XERJ should move the field-report template link out of '## Optional' into 'Start here' (as step 4's target) or a '## Before you finish' section, and leave '## Optional' for genuinely optional links.
- Bun's feedback page front-loads a 'try upgrade first' step ('Upgrade Bun to the latest version with `bun upgrade`. This might fix your problem without opening an issue.'). XERJ's field-report ask could mirror this with 'Run `xerj feedback --dry-run` first; it auto-fills version/OS/what-was-indexed' as the very first instruction, lowering the cost of the required report.
- Negative finding worth recording for the coordinator: Bun (96k stars) has NO agent prompt, NO MCP, NO one-click links and NO contribution ask in its llms files -- its llms.txt is a plain Mintlify auto-generated docs index. Do not cite Bun as a model for agent-install prompts; cite it only for catalogue brevity, the verify step, multi-channel install, and the bun-init auto-written CLAUDE.md pattern.
