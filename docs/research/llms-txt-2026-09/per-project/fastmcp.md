# FastMCP (jlowin/fastmcp, gofastmcp.com)

Category: MCP framework (Python)

Dissected 2026-09-18 by one agent pass fetching the live URLs below. Quotes are verbatim as returned; see ../README.md for what was and was not independently re-checked.

## llms.txt

- URL: <https://gofastmcp.com/llms.txt> — HTTP 200
- Size: 64883 bytes, 574 lines
- Install position: line 6 of 574, the 2nd link in the file: "- [Installation](https://gofastmcp.com/getting-started/installation.md): Install FastMCP and verify your setup". No install command appears in llms.txt itself; it is a pointer-only index. MCP-client install pages appear at lines 112-125 (integrations/claude-code, cursor, gemini-cli, goose; cli/install-mcp).
- Tone: llms.txt: neutral, auto-generated Mintlify docs index — one tagline ("> The fast, Pythonic way to build MCP servers and clients."), then link+description lines; it makes NO demands of the agent, contains no imperative instructions, no prompts, no commands. llms-full.txt: neutral tutorial voice ("That's it! You've created a FastMCP server, albeit a very boring one."), with occasional <Warning> blocks. The only demanding/imperative voice is in the Contributing page and CLAUDE.md, and there it is a GATE against agents (LLM-generated issues closed immediately; issue-first, no exceptions) rather than a request for agent contributions.
- Section order: # FastMCP (single H1; blockquote tagline; then ONE flat bullet list of ~570 links with no ## headings) → (implied by URL path, in order) getting-started: welcome, installation, quickstart, whats-new → servers/* (server, tools, resources, prompts, context, transforms, providers, composition, elicitation, sampling, middleware, auth ...) → clients/* (incl. client-only-package, fastmcp-remote) → integrations/* (claude-code, claude-desktop, cursor, gemini-cli, goose, chatgpt, openai, anthropic, auth providers) → cli/* (incl. cli/install-mcp) → deployment/* → development/* (contributing, releases, tests) → patterns/* (contrib) → more/faq → python-sdk/* (auto-generated API reference, ~170 entries incl. fastmcp-cli-install-{claude_code,claude_desktop,cursor,gemini_cli,goose,mcp_json,shared,stdio}) → v3/* mirror of the above (lines ~369-497) → v2/* mirror of the above (lines ~498-574)

### Install commands found

```sh
(none in llms.txt itself — all below are from llms-full.txt / installation.md)
uv add fastmcp
pip install fastmcp
pip install "fastmcp[tasks]"
fastmcp version   (the verify step)
fastmcp install claude-desktop server.py
fastmcp install claude-code server.py --with pandas --with matplotlib
fastmcp install cursor server.py --with-editable .
fastmcp install claude-code server.py
fastmcp install cursor server.py --workspace .
fastmcp install mcp-json server.py
fastmcp install stdio server.py   # Output: uv run --with fastmcp fastmcp run /absolute/path/to/server.py
claude mcp add dice-roller -- uv run --with fastmcp fastmcp run server.py
claude mcp add my-server --scope user -- uv run --with fastmcp fastmcp run server.py
pip install --force-reinstall fastmcp   (troubleshooting)
brew install uv   (prerequisite warning)
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

```text
{
  "mcpServers": {
    "dice-roller": {
      "command": "uv",
      "args": [
        "run",
        "--with", "fastmcp",
        "--with", "pandas",
        "--with", "requests", 
        "fastmcp",
        "run",
        "path/to/your/server.py"
      ]
    }
  }
}

(source: https://gofastmcp.com/integrations/claude-desktop in llms-full.txt; followed by: "After updating the configuration file, restart Claude Desktop completely. Look for the hammer icon (🔨) to confirm your server is loaded.")

Generic form from https://gofastmcp.com/cli/install-mcp:
{
  "server-name": {
    "command": "uv",
    "args": ["run", "--with", "fastmcp", "fastmcp", "run", "/path/to/server.py"],
    "env": {
      "API_KEY": "value"
    }
  }
}
```

### Feedback / contribution ask (verbatim)

```text
(nothing in llms.txt beyond the link "- [Contributing](https://gofastmcp.com/development/contributing.md): Development workflow for FastMCP contributors". From llms-full.txt, development/contributing:)

**Every pull request requires a corresponding issue - no exceptions.** This requirement creates a collaborative space where approach, scope, and alignment are established before code is written.

FastMCP is an extremely highly-trafficked repository maintained by a very small team. Issues that appear to transfer burden to maintainers without any effort to validate the problem will be closed. Please help the maintainers help you by always providing a minimal reproducible example and clearly describing the problem.

**LLM-generated issues will be closed immediately.** Issues that contain paragraphs of unnecessary explanation, verbose problem descriptions, or obvious LLM authorship patterns obfuscate the actual problem and transfer burden to maintainers.

Write clear, concise issues that:

* State the problem directly
* Provide a minimal reproducible example
* Skip unnecessary background or context
* Take responsibility for clear communication
```

### Best passage for driving install and first use (verbatim)

````text
### Verify Installation

To verify that FastMCP is installed correctly, you can run the following command:

```bash
fastmcp version
```

You should see output like the following:

```bash
$ fastmcp version

FastMCP version:                           4.0.0
MCP version:                                2.0.0
Python version:                            3.12.2
Platform:            macOS-15.3.1-arm64-arm-64bit
FastMCP root path:            ~/Developer/fastmcp
```

(source: https://gofastmcp.com/getting-started/installation — the page llms.txt entry 2 points to)
````

### One-click links

- `(no literal cursor:// or vscode: URL appears in llms.txt or llms-full.txt; deeplinks are generated by the CLI, described in prose:)`
- `llms-full.txt Supported Clients table: "| `cursor`         | Deeplink that opens Cursor for confirmation             |" and "| `goose`          | Deeplink that opens Goose for confirmation (uses `uvx`) |"`
- `llms-full.txt line 36617: "The easiest way to install a FastMCP server in Goose is using the `fastmcp install goose` command. This generates a `goose://` deeplink and opens it, prompting Goose to install the server."`
- `llms-full.txt integrations/claude-code: "The command will automatically configure the server with Claude Code's `claude mcp add` command."`
- `claude mcp add dice-roller -- uv run --with fastmcp fastmcp run server.py`

## llms-full.txt

- URL: <https://gofastmcp.com/llms-full.txt> — HTTP 200
- Size: 2885378 bytes
- Relationship to llms.txt: Mintlify-generated concatenation of every .md page that llms.txt links to (including the v3 and v2 mirrors, hence 2.9 MB / 60,835 lines). Same content as the docs site, not curated; Mintlify JSX components (<Warning>, <Tip>, <CodeGroup>, <VersionBadge />, <video />) and fence metadata (theme={...}) are left in verbatim. llms.txt = index of the pages; llms-full.txt = their bodies.
- Install position: Installation page ("# Installation\nSource: https://gofastmcp.com/getting-started/installation") is NOT near the top: the file opens with "# Architecture" (apps docs) and Installation appears after Apps/Interactive Tools/Auth sections; found via grep. Each page is delimited by an H1 plus a "Source: <url>" line; 1,276 H1 lines total (many are code comments, not pages). It also contains the full pages for cli/install-mcp, integrations/claude-code, integrations/claude-desktop, integrations/cursor, integrations/goose and development/contributing.

## Other agent-facing files

### CLAUDE.md — HTTP 200

<https://raw.githubusercontent.com/jlowin/fastmcp/main/CLAUDE.md>

Contributor-agent guide (21,741 bytes, 214 lines): required dev workflow, repo map, git/CI rules, release process, commit/PR structure, code-review rules. It is about contributing TO FastMCP, not about installing or using it; contains no install-for-users text.

```text
> **Audience**: LLM-driven engineering agents and human developers

> **Note**: `AGENTS.md` is a symlink to this file. Edit `CLAUDE.md` directly.
[...]
### Commit Messages and Agent Attribution

- **Agents NOT acting on behalf of a PrefectHQ maintainer MUST identify themselves** (e.g., "🤖 Generated with Claude Code" in commits/PRs)
- Keep commit messages brief - ideally just headlines, not detailed messages
[...]
### PR Messages - Required Structure

- 1-2 paragraphs: problem/tension + solution (PRs are documentation!)
- Focused code example showing key capability
- **Avoid:** bullet summaries, exhaustive change lists, verbose closes/fixes, marketing language
- No "test plan" sections or testing summaries
```

### AGENTS.md — HTTP 200

<https://raw.githubusercontent.com/jlowin/fastmcp/main/AGENTS.md>

9-byte symlink/pointer whose entire content is the filename of CLAUDE.md (raw fetch returns the literal text of the symlink target).

```text
CLAUDE.md
```

### llms-install.md — HTTP 404

<https://raw.githubusercontent.com/jlowin/fastmcp/main/llms-install.md (also tried https://gofastmcp.com/llms-install.md)>

Does not exist.

### SKILL.md — HTTP 404

<https://raw.githubusercontent.com/jlowin/fastmcp/main/SKILL.md (also tried https://gofastmcp.com/SKILL.md)>

Does not exist at repo root or docs root. (Note: the docs DO have a 'Skills Provider' page — https://gofastmcp.com/servers/providers/skills.md: "Expose agent skills as MCP resources" — which is a feature for serving skills, not a skill for FastMCP itself.)

### MCP install page (Install MCP Servers) — HTTP 200

<https://gofastmcp.com/cli/install-mcp (body read from llms-full.txt)>

One CLI verb, `fastmcp install <client> server.py`, with a 7-row Supported Clients table (claude-code, claude-desktop, cursor, gemini-cli, goose, mcp-json, stdio) — each client gets its own install mechanism (built-in MCP management, config-file edit, deeplink, or emitted JSON/shell command for hosts it does not know).

```text
`fastmcp install` registers a server with an MCP client application so the client can launch it automatically. Each MCP client runs servers in its own isolated environment, which means dependencies need to be explicitly declared — you can't rely on whatever happens to be installed locally.
[...]
| Client           | Install method                                          |
| ---------------- | ------------------------------------------------------- |
| `claude-code`    | Claude Code's built-in MCP management                   |
| `claude-desktop` | Direct config file modification                         |
| `cursor`         | Deeplink that opens Cursor for confirmation             |
| `gemini-cli`     | Gemini CLI's built-in MCP management                    |
| `goose`          | Deeplink that opens Goose for confirmation (uses `uvx`) |
| `mcp-json`       | Generates standard MCP JSON config for manual use       |
| `stdio`          | Outputs the shell command to run via stdio              |
```

### Claude Code integration page — HTTP 200

<https://gofastmcp.com/integrations/claude-code (body read from llms-full.txt)>

Create server -> install (CLI or manual `claude mcp add`) -> 'Using the Server' section that shows the first prompt to type and the expected reply, so the reader can confirm the install end-to-end without a shell command.

```text
## Using the Server

Once your server is installed, you can start using your FastMCP server with Claude Code.

Try asking Claude something like:

> "Roll some dice for me"

Claude will automatically detect your `roll_dice` tool and use it to fulfill your request, returning something like:

> I'll roll some dice for you! Here are your results: \[4, 2, 6]
[...]
If your server provides resources, you can reference them with `@` mentions using the format `@server:protocol://resource/path`. If your server provides prompts, you can use them as slash commands with `/mcp__servername__promptname`.
```

## What they do better than XERJ

- Verification is a named, first-class step with the EXPECTED OUTPUT printed: '### Verify Installation ... fastmcp version ... You should see output like the following:' followed by a 5-line sample. XERJ's llms.txt has no 'you should see' block anywhere in Start here; an agent cannot tell a good install from a bad one without inventing a check.
- Per-client MCP install matrix: seven install targets in one table (claude-code / claude-desktop / cursor / gemini-cli / goose / mcp-json / stdio), each with its mechanism named. XERJ's `xerj init` is described as 'registers the MCP server in `.mcp.json` and writes a 10-line skill for Claude Code/Cursor' with no table, no per-client JSON, and no 'mcp-json'-style escape hatch for unknown hosts.
- A 'no-shell' confirmation path: the Claude Code page ends with a prompt to type ('Roll some dice for me') and the expected reply; the Claude Desktop page says 'Look for the hammer icon (🔨) to confirm your server is loaded.' — success criteria that a shell-less persona can apply. XERJ's shell-less path is one link ('Agent quickstart ... the HTTP-only path') with no in-file success criterion.
- Verbatim, copy-pasteable MCP JSON with `command`/`args`/`env` shown for both the simple and the with-dependencies case, and a generated form (`fastmcp install mcp-json server.py`) for CI/CD or unsupported clients. XERJ's llms.txt has zero MCP config JSON; it links a tools schema instead.
- Prerequisite warnings are inline <Warning> blocks placed right at the command that needs them ('`uv` must be installed and available in your system PATH ... `brew install uv`'; '**Claude Code must be installed**. The integration looks for the Claude Code CLI at the default installation location (`~/.claude/local/claude`)'). Same pattern as XERJ's PATH warning, but applied to every prerequisite, not one.
- The index itself is short per-entry and neutral: one line per page, no policy prose, so retrieval by an agent is cheap and predictable. XERJ's Start here paragraphs run 400-1000 characters per bullet.
- Troubleshooting is scoped honestly: 'This affects one specific case: upgrading to FastMCP 3.3 or later from FastMCP 3.2 or earlier with `pip`. Fresh installs and `uv` upgrades are unaffected, so you can skip this unless you did exactly that.' — tells the reader when NOT to read on.

## What XERJ does better

- XERJ's llms.txt is authored for the agent as reader (sections Start here / Caveats / Core agent operations / Answers), with commands inline; FastMCP's llms.txt is a plain auto-generated Mintlify docs index — 574 link lines, one H1, no ## headings, no commands, nothing about installation beyond the pointer 'Install FastMCP and verify your setup'. An agent that only reads FastMCP's llms.txt learns nothing actionable.
- XERJ states install-time honesty facts inline (SHA-256 fail-closed, PATH not edited, data-dir-outside-indexed-tree, exit code 3 = success). FastMCP has no equivalent security/integrity statements for its install.
- XERJ has an explicit feedback mechanism with a one-line command (`xerj feedback --open-pr --agent ... --verdict ...`), a dry-run, a sandboxed fallback, and 'A negative report is a good report.' FastMCP has no agent-feedback ask at all; its Contributing page actively repels agent-authored issues ('**LLM-generated issues will be closed immediately.**').
- XERJ's caveats block (lexical-not-neural default, ingest-time embed mode, measured kNN recall, RSS-runaway defect) is unmatched; FastMCP's llms-full.txt has no 'what this does not do' section for first-time readers.
- XERJ links machine-readable tool schemas (mcp-tools.json, openai-tools.json, anthropic-tools.json) generated from a real `tools/list`; FastMCP publishes no such artifact.
- XERJ's llms-full.txt is curated (84 KB) rather than a 2.9 MB concatenation that duplicates v2 and v3 docs; FastMCP's llms-full opens with '# Architecture' of the Apps subsystem, not with installation.

## Adoptable ideas

- Add a 'Verify installation' block modelled on https://gofastmcp.com/getting-started/installation: '### Verify Installation / To verify that FastMCP is installed correctly, you can run the following command: `fastmcp version` / You should see output like the following:' + sample output. XERJ equivalent: after the installer line in Start here, add `xerj --version` (or `$HOME/.local/bin/xerj --version`) and `curl -s http://localhost:9200/` with the literal expected JSON, and for the shell-less persona `GET /_cluster/health` with the expected `"status":"green"`. The 22-run study's 'verified-install flipped a blocked persona' finding maps directly onto this section.
- Add the seven-target install table from https://gofastmcp.com/cli/install-mcp ('| Client | Install method |' with claude-code / claude-desktop / cursor / gemini-cli / goose / mcp-json / stdio) as a 'Wire XERJ into your agent' table under Start here: `xerj init` (Claude Code + Cursor, writes `.mcp.json`), `claude mcp add xerj -- xerj mcp --url http://localhost:9200`, a verbatim `"mcpServers": { "xerj": { "command": "xerj", "args": ["mcp"] } }` block for Claude Desktop/other hosts, and a 'stdio' row that prints the bare command. This fixes 'the MCP server exists but was not shipped/documented' with 10 lines.
- Copy the 'Using the Server' pattern from https://gofastmcp.com/integrations/claude-code — 'Try asking Claude something like: > "Roll some dice for me" / Claude will automatically detect your `roll_dice` tool and use it ... returning something like: > I'll roll some dice for you! Here are your results: [4, 2, 6]'. XERJ version: 'Try asking: "search the index for BuiltinPager" — the agent should call `xerj_search` and return file:line + passage like: ...'. It gives shell-less/MCP-only personas a success criterion that is not a shell command.
- Adopt the 'Look for the hammer icon (🔨) to confirm your server is loaded.' style of non-shell confirmation for each host: after `xerj init`, state what the agent will observe (tool names `xerj_search`, `xerj_autoindex`... appearing in `tools/list`; the slash command `/mcp__xerj__...`).
- Adopt inline prerequisite <Warning>s at the command that needs them, per https://gofastmcp.com/cli/install-mcp: '`uv` must be installed and available in your system PATH.' and per integrations/claude-code: '**Claude Code must be installed**. The integration looks for the Claude Code CLI at the default installation location (`~/.claude/local/claude`)'. XERJ: put 'a node must already be running at :9200' and 'gh must be authenticated' warnings immediately beside `xerj autoindex` and `xerj feedback --open-pr`, not paragraphs earlier.
- Provide an `mcp-json`-style escape hatch (https://gofastmcp.com/cli/install-mcp: 'The `mcp-json` target generates standard MCP configuration JSON instead of installing into a specific client. This is useful for clients that FastMCP doesn't directly support, for CI/CD environments, or for sharing server configs' + `--copy` to clipboard). XERJ: `xerj init --print-json` (or document the literal JSON) so hosts other than Claude Code/Cursor can be wired without a shell.
- Fix the 'required vs Optional' contradiction using FastMCP's structural cue: FastMCP puts the demand where the reader is ('**Every pull request requires a corresponding issue - no exceptions.**' sits under a heading named '### Issue First, Code Second'). Rename XERJ's `## Optional` heading to something like `## Your side of the deal (field report — required)` or move the field-report bullet up into Start here step 4 and leave only truly optional links under Optional. This is an inference from the user's stated concern, not something FastMCP's file says.
- Scope troubleshooting honestly the way https://gofastmcp.com/getting-started/installation does: 'This affects one specific case: ... Fresh installs and `uv` upgrades are unaffected, so you can skip this unless you did exactly that.' Apply the same 'skip this unless' phrasing to XERJ's PATH warning and to the lexical-vs-neural caveat, so agents stop over-applying them (9 of 12 misread the embedder; a 'you are in lexical mode unless you passed --embed-mode neural — skip the rest unless you did' opener is the FastMCP-style fix).
