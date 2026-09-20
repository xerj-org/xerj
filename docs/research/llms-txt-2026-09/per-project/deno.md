# Deno

Category: cli/runtime

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://docs.deno.com/llms.txt> — HTTP 200
- Size: 5055 bytes, 60 lines
- Install position: absent as a command. llms.txt contains NO install command at all. Installation is delegated: line 10 links agents.md ('Entrypoint for coding agents working in a user's project — orientation, the Node assumptions to unlearn, and how to install Deno's agent skills'), and the Runtime section links 'Getting Started' and 'CLI Reference'. The word 'install' appears only inside those link descriptions.
- Tone: neutral / descriptive index. Makes no demands of the agent; zero imperatives. The only agent-directed framing is the description of agents.md as the 'Entrypoint for coding agents' and the Optional-section link 'AI Skills for Coding Assistants'.
- Section order: (preamble: one-line blockquote description + 4 pointer links: agents.md, llms-full-guide.txt, llms-summary.txt, llms-full.txt) → ## Runtime → ## Deploy → ## Sandbox → ## Examples → ## Optional

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

_none_

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

```text
- [agents.md](https://deno.com/agents.md): Entrypoint for coding agents working in a user's project — orientation, the Node assumptions to unlearn, and how to install Deno's agent skills (https://github.com/denoland/skills), which carry the full reference material
- [llms-full-guide.txt](https://docs.deno.com/llms-full-guide.txt): Complete agent-oriented guide with CLI reference, code examples, and usage patterns (publish alongside this file)
- [llms-summary.txt](https://docs.deno.com/llms-summary.txt): Compact index of all documentation sections
- [llms-full.txt](https://docs.deno.com/llms-full.txt): Full documentation content dump (large)
```

## llms-full.txt

- URL: <https://docs.deno.com/llms-full.txt> — HTTP 200
- Size: 2497222 bytes
- Relationship to llms.txt: Auto-generated full concatenation of every docs page (header SAYS: 'This document contains the full content of the Deno documentation website.'). Each page is '# Title' + '> description' + 'URL: https://docs.deno.com/...' + body, separated by '---'. Ordered alphabetically by page title, NOT by importance — it opens with 'Acceptable use policy' and the Deploy changelog; 'Get started with Deno' (the page with the agent prompt) is at line 47995 of 76755. Raw MDX leaks through (<deno-tabs>, :::tip). Not curated. Deno ALSO publishes two curated siblings not present in XERJ's layout: llms-full-guide.txt (200, 5,810 B, 204 lines — a hand-written agent-oriented CLI cheat-sheet: 'CLI quick reference', 'Permissions', 'Project configuration', ..., 'Key documentation links') and llms-summary.txt (200, 26,505 B, 193 lines — one line per page with its description).
- Install position: Line 48026 '## Using an AI coding agent?' then line 48044 '## Install Deno' (curl -fsSL https://deno.land/install.sh | sh at line 48050, irm ... at 48058), inside the 'Get started with Deno' page (URL: https://docs.deno.com/runtime/), i.e. ~63% of the way into the file. The install curl also appears at lines 2461, 3705, 3719, 47206-47270 (other pages). A second agent pointer at line 48455 in the 'Migrate from Node.js' page: ':::tip Using an AI coding agent? Point it at deno.com/agents.md. It orients the agent and installs Deno's `migrate-to-deno` skill'. No MCP content anywhere in the file (grep -i mcp: zero hits). No one-click links except line 68251 'vscode:extension/denoland.vscode-deno'. Feedback CTAs are product-support ones only (line 2775 'please report your feedback at https://github.com/denoland/deploy_feedback/issues/'; line 11854 '# Support and Feedback' for Deploy) — there is no agent report-back ask.

## Other agent-facing files

### agents.md — HTTP 200

<https://deno.com/agents.md>

Single 123-line, 4,967 B page written for coding agents dropped into a user's project: what Deno is, 'Assumptions to drop' (5 Node misconceptions), '## 1. Get the CLI' (version check, then ask-before-install with 4 install commands), '## 2. Get the skill' (dx skills add / Claude Code plugin / read-the-raw-URL fallback), '## 3. Reading the docs' (--help is authoritative, append .md to any docs URL, llms.txt), '## 4. If you were asked to adopt Deno' (read-only investigation first, opt-in steps, never one big migration). Identical bytes served at https://docs.deno.com/agents.md (200). No report-back ask of any kind.

````text
## 1. Get the CLI

```sh
deno --version
```

If that prints 2.9 or later you are set. If it prints something older, run
`deno upgrade`. If the command is missing, tell the user you would like to
install Deno, show them how, and run one of these only once they agree:

```sh
# macOS / Linux
curl -fsSL https://deno.land/install.sh | sh
# Windows (PowerShell)
irm https://deno.land/install.ps1 | iex
# Homebrew (macOS / Linux)
brew install deno
# npm
npm install -g deno
```

Deno installs as a standalone binary. It adds nothing to the project and no
dependency to `package.json`.
````

### agents.md — skill install + no-install fallback — HTTP 200

<https://deno.com/agents.md>

The section that answers the shell-less / policy-constrained persona: install the skill only with consent, and if you cannot install, read the raw markdown at a stable URL.

````text
If the user agrees, install them. This touches no project source and adds no
dependency:

```sh
dx skills add denoland/skills --skill deno
```

`dx` is Deno's `npx` equivalent and ships with the binary you installed above,
so this needs no Node. If you would rather use Node, `npx skills add ...` is
identical.

Add `--skill migrate-to-deno` as well if the project is coming from Node, npm,
Yarn, pnpm, or Bun. In Claude Code you can instead install the whole set as a
plugin:

```
/plugin marketplace add denoland/skills
/plugin install deno-skills@denoland-skills
```

Skip this if an equivalent Deno skill or rule is already present.

**If you cannot install skills**, read them directly — they are plain markdown,
and these URLs always serve the current version:

- <https://raw.githubusercontent.com/denoland/skills/main/skills/deno/SKILL.md>
- <https://raw.githubusercontent.com/denoland/skills/main/skills/migrate-to-deno/SKILL.md>
````

### agents.md — adoption protocol — HTTP 200

<https://deno.com/agents.md>

Behavioural guardrail for the 'set up Deno in this project' prompt: read-only first, present opt-in steps, let the user choose in one round.

```text
Most Node projects already run under Deno unchanged, so this is a series of
small opt-in wins, not a rewrite. **Never propose one big migration**, and do
not start editing.

Investigate read-only first — dependencies, scripts, the lockfile, the
TypeScript runner, the test and lint setup, CI. Then present what you found as
independent, opt-in steps ordered by how little they disturb, and let the user
choose in one round.
```

### agents.md — docs access for agents — HTTP 200

<https://deno.com/agents.md>

How an agent reads docs without a browser: --help is authoritative, every docs page has a .md twin.

```text
`deno <subcommand> --help` is authoritative and version-accurate — check it
before guessing at a flag.

Beyond that:

- <https://docs.deno.com/llms.txt> — index of the documentation
- Any docs page also serves its markdown source: append `.md` to the URL, as in
  <https://docs.deno.com/runtime/fundamentals/security.md>
- <https://docs.deno.com/api/> — the `Deno.*` API reference
- `deno doc jsr:@std/path` — a package's API without leaving the terminal
```

### llms-full-guide.txt — HTTP 200

<https://docs.deno.com/llms-full-guide.txt>

Curated 204-line (5,810 B) agent cheat-sheet, distinct from the 2.5 MB dump: CLI quick reference table, permissions, deno.json, module resolution, Node compat, testing, HTTP server, Deploy, Sandbox, key links. No install command, no prompt, no feedback ask. Ends with 'AI skills: https://github.com/denoland/skills'.

````text
## CLI quick reference

```
deno run main.ts          # run a script (sandboxed by default)
deno run -A main.ts       # run with all permissions
deno test                 # run tests (*_test.ts, *.test.ts)
deno fmt                  # format code
deno lint                 # lint code
deno task <name>          # run a task defined in deno.json
deno add <package>        # add a dependency to deno.json
deno init my_project      # scaffold a new project
```
````

### llms-summary.txt — HTTP 200

<https://docs.deno.com/llms-summary.txt>

193-line (26,505 B) middle tier: one line per docs page with full description. Header SAYS: 'A compact, LLM-friendly overview of the Deno docs. For a full index, use llms.txt. For full content, use llms-full.txt.'

```text
> A compact, LLM-friendly overview of the Deno docs. For a full index, use llms.txt. For full content, use llms-full.txt.
```

### skills/deno/SKILL.md — HTTP 200

<https://raw.githubusercontent.com/denoland/skills/main/skills/deno/SKILL.md>

281-line (10,906 B) Agent-Skills-spec skill (frontmatter: name, description, license MIT, version 1.0). Sections: 'Deno works the way npm and bun do', Dependency management, Permissions, Configuration, Packages, Built-in tooling, Running code, Starting a new project, Publishing, 'Reviewing Deno code' (a checklist of smells), Further reading. Repeats the agents.md misconceptions so the context persists across sessions.

```text
Needs Deno 2.9+. Check with `deno --version`, update with `deno upgrade`.

## Deno works the way npm and bun do

Deno is not a separate ecosystem to port code into:

- `deno install` reads an existing `package.json` and writes a real
  `node_modules`.
- `deno add express` installs from **npm**. Unprefixed names default to npm.
[...]
Don't tell users to rewrite imports, adopt JSR, or restructure as a
precondition. The two real differences are **permissions** and **npm lifecycle
scripts not running by default**.
```

### denoland/skills README.md — HTTP 200

<https://raw.githubusercontent.com/denoland/skills/main/README.md>

180-line skills repo README: skills table, 'Key Principles', SemVer policy, and '## Installation' with three tiers for Claude Code (plugin marketplace, npx skills, manual cp into ~/.claude/skills/) plus Cursor. Contributor ask is versioning/conventional-commits only — no field-report or feedback ask.

````text
**Option 1: Install as a plugin**

```bash
# Step 1: Add the marketplace
/plugin marketplace add denoland/skills

# Step 2: Install the plugin
/plugin install deno-skills@denoland-skills
```

**Option 2: `npx skills`**

Works across Claude Code, Cursor, Copilot, and other skills-compatible agents:

```bash
# All skills
npx skills add denoland/skills

# Or just one
npx skills add denoland/skills --skill deno
```

**Option 3: Manual installation**
````

### CLAUDE.md (denoland/deno repo) — HTTP 200

<https://raw.githubusercontent.com/denoland/deno/HEAD/CLAUDE.md>

433-line (11,866 B) contributor-facing 'Deno Development Guide' for agents hacking on the Deno codebase itself: Git workflow, build commands, testing, spec tests, debugging, codebase navigation, troubleshooting. Not user-installation material. Its closing 'Getting Help' is the only feedback-adjacent text.

```text
- When pushing updates to the PR, make sure to never force push. Create as many
  commits as you need, all of them get squashed when the PR is merged, so there
  is no need to rewrite history.
[...]
### Getting Help

- Check existing issues on GitHub
- Look at recent PRs for similar changes
- Review the Discord community for discussions
- When in doubt, ask! The maintainers are helpful
```

### llms-install.md — HTTP 404

<https://docs.deno.com/llms-install.md>

Does not exist (HTML 404 page returned).

### AGENTS.md (deno repo root) — HTTP 404

<https://raw.githubusercontent.com/denoland/deno/HEAD/AGENTS.md>

Does not exist in the repo; the agent entrypoint lives on the website (deno.com/agents.md), not in the repo.

### MCP install page — HTTP 404

<https://docs.deno.com/mcp.md>

No MCP page exists; grep of llms-full.txt for 'mcp' returns zero hits. Deno ships no MCP server and does not document one — its agent integration is skills + a markdown page, not MCP.

### deno.com/llms.txt (marketing root) — HTTP 200

<https://deno.com/llms.txt>

Soft 404: HTTP 200 but body is the Fresh-rendered HTML 'Not Found | Deno' page (58,818 B). The real llms.txt lives only under docs.deno.com.

## What they do better than XERJ

- Brevity and hierarchy: llms.txt is 60 lines / 5 KB and delegates; agents.md is 123 lines with four numbered steps. XERJ's llms.txt is 234 lines / 40 KB with multi-paragraph Caveats inline and duplicated step numbering ('4.' twice at lines 23-24).
- Consent-first install: 'tell the user you would like to install Deno, show them how, and run one of these only once they agree' + 'adds nothing to the project'. XERJ's Start here presents `curl | sh` as step zero with no consent framing (the manual verified-install path is one link deep).
- An explicit fallback for agents that cannot install: '**If you cannot install skills**, read them directly — they are plain markdown, and these URLs always serve the current version'. XERJ's shell-less path exists (docs/agents/quickstart) but is a link description, not a bold conditional in the flow.
- Behavioural guardrails in the entrypoint: 'Never propose one big migration, and do not start editing. Investigate read-only first ... let the user choose in one round.' XERJ has the estimate-first / --approve gate but only as a linked answer page.
- Skills distributed via the Agent Skills Specification through three channels (dx/npx skills add, Claude Code /plugin marketplace, manual cp), so the product context persists across sessions without a running server. XERJ's skill is only written by `xerj init` on a machine that already has the binary.
- Four-tier llms file set (index / summary / curated guide / full dump) with each file stating what the others are for. XERJ has two.
- 'Append .md to any docs URL' rule stated once, giving shell-less agents a universal markdown fetch path.
- The paste prompt names six agents ('Claude Code, Codex, Gemini CLI, OpenCode, Cursor, Pi') and is a plain sentence, not a command — works for every persona including MCP-only.

## What XERJ does better

- Honesty caveats inline: XERJ's '## Caveats (read before you rely on a capability)' names the lexical-vs-neural default, ingest-time embedding, RSS growth (20.2 GB data point), and _source size (416,630 B → 12,122 B measured). Deno's llms.txt has no caveats section at all; its only misconception list is Node-migration folklore.
- Supply-chain honesty on the installer: XERJ documents SHA-256 fail-closed verification, the checksum-tool fallback chain, XERJ_INSECURE_SKIP_CHECKSUM=1 opt-in, and 'the manual verifiable install without curl | sh'. Deno's agents.md just says `curl -fsSL https://deno.land/install.sh | sh` with no integrity statement.
- PATH-consequence paragraph ('in a fresh shell ~/.local/bin is often not on PATH and a bare xerj … fails with command not found') — Deno's agents.md does not warn about post-install PATH at all.
- Machine-readable tool schemas (mcp-tools.json, anthropic-tools.json 'generated from a real tools/list of the server that serves it') and an MCP stdio server in the binary. Deno has zero MCP content (grep: 0 hits).
- Run-it-for-a-human protocol: exit-code table ('3 completed-with-junk is SUCCESS'), progress lines to relay verbatim, --dry-run estimate before occupying a laptop, --max-minutes/--approve gate. Deno has nothing comparable because its operations are short.
- A defined feedback loop at all: `xerj feedback --open-pr` (one-file CLA-exempt PR), a dry-run for sandboxed agents, an obligation-transfers-to-operator clause, 'a negative report is a good report'. Deno asks agents for nothing back — so Deno is not a model for fixing the PR-volume problem, only for the install/onboarding half.
- Answer pages written as questions ('Do I even need embeddings for search, or is full-text enough?') mapped one-to-one to agent intents; Deno's index is by product area.

## Adoptable ideas

- A one-line, product-neutral paste prompt on the Getting Started page and in llms-full.txt, verbatim from https://docs.deno.com/runtime/ (llms-full.txt line 48026): '## Using an AI coding agent?  Paste this into Claude Code, Codex, Gemini CLI, OpenCode, Cursor, Pi, or whichever agent you use:  Read deno.com/agents.md and set up Deno in this project'. XERJ equivalent: '## Using an AI coding agent?  Paste this into Claude Code, Codex, Gemini CLI, OpenCode, Cursor, or whichever agent you use:  Read xerj.org/agents.md and make this folder searchable' — a bare URL + verb, no flags, no curl in the prompt.
- Split the agent entrypoint out of llms.txt into a short numbered agents.md (Deno's is 123 lines / 4,967 B vs XERJ's 234-line / 40 KB llms.txt). Deno's llms.txt SAYS only: '- [agents.md](https://deno.com/agents.md): Entrypoint for coding agents working in a user's project — orientation, the Node assumptions to unlearn, and how to install Deno's agent skills'. Structure to copy: '## Assumptions to drop' → '## 1. Get the CLI' → '## 2. Get the skill' → '## 3. Reading the docs' → '## 4. If you were asked to adopt'. XERJ's 'Caveats' block (lexical-not-neural) is exactly an 'Assumptions to drop' list — 9 of 12 agents misreading the embedder is the same failure class Deno addresses with 'Don't tell users to rewrite imports, adopt JSR, or restructure as a precondition'.
- Consent-gated install wording, verbatim from agents.md: 'If the command is missing, tell the user you would like to install Deno, show them how, and run one of these only once they agree' and 'Deno installs as a standalone binary. It adds nothing to the project and no dependency to `package.json`.' XERJ's 'Start here' jumps straight to `curl -fsSL https://xerj.org/get | sh`; the policy-constrained persona that flipped to success needs this sentence plus the 'adds nothing to the project' reassurance (XERJ can truthfully say: one binary in ~/.local/bin, nothing written into the repo unless you run `xerj init`).
- Explicit read-instead-of-install fallback with stable raw URLs, verbatim from agents.md: '**If you cannot install skills**, read them directly — they are plain markdown, and these URLs always serve the current version: https://raw.githubusercontent.com/denoland/skills/main/skills/deno/SKILL.md'. XERJ should publish its `xerj init` 10-line skill as a standalone SKILL.md at a raw GitHub URL and say '**If you cannot run `xerj init`**, read it directly'; and for the shell-less MCP-only persona, the parallel sentence '**If you have no shell**, everything under Core agent operations is plain HTTP — start at https://xerj.org/docs/agents/quickstart' promoted from a link description to a bold conditional.
- Ship the skill through the standard channels Deno lists so the agent can install context without touching the project, verbatim from agents.md: 'dx skills add denoland/skills --skill deno' and 'In Claude Code you can instead install the whole set as a plugin: /plugin marketplace add denoland/skills / /plugin install deno-skills@denoland-skills' and 'Skip this if an equivalent Deno skill or rule is already present.' XERJ: publish xerj-org/skills following the Agent Skills Specification (https://agentskills.io/specification) so `npx skills add xerj-org/skills --skill xerj` works, and list the Claude Code /plugin pair.
- Tiered llms files instead of one 40 KB index: Deno publishes llms.txt (5 KB index), llms-summary.txt (26 KB, one line per page), llms-full-guide.txt (5.8 KB curated CLI cheat-sheet: 'Complete agent-oriented guide with CLI reference, code examples, and usage patterns (publish alongside this file)'), and llms-full.txt (2.5 MB dump). XERJ's llms-full.txt (84 KB) already plays the 'guide' role; the adoptable move is to cut llms.txt down to a Deno-sized index that points at agents.md first, and move the inline Caveats/protocol prose into the guide tier.
- Every docs page serves its markdown twin, verbatim from agents.md: 'Any docs page also serves its markdown source: append `.md` to the URL, as in https://docs.deno.com/runtime/fundamentals/security.md'. XERJ already serves .md for /answers and /compare (llms.txt links end in .md) but /docs/* links do not — extend the rule to every page and state it in one sentence so a shell-less agent can WebFetch anything.
- Version-check-first, upgrade-if-old pattern for the first command, verbatim: 'deno --version  If that prints 2.9 or later you are set. If it prints something older, run `deno upgrade`.' XERJ's first-run list starts the node before checking anything; a `xerj --version` gate with the minimum release that has `xerj feedback` / `--approve` (llms.txt already notes 'which releases have it') would let agents branch correctly instead of hitting missing subcommands.
- Put the report ask where the agent is, not under 'Optional'. INFER (Deno has no report ask at all, so this is a contrast, not a copy): Deno's only imperative section headings are numbered steps '## 1. Get the CLI' ... '## 4.'; XERJ's field-report step is numbered '4.' twice (duplicate '4.' in Start here, lines 23-24) and its template lives under '## Optional' — a heading Deno reserves for 'Contributing', 'Style Guide', 'Subhosting'. Renaming XERJ's section to '## Your side of the deal' (the phrase already used at line 225) and fixing the duplicate step number removes the contradiction the user flagged.
