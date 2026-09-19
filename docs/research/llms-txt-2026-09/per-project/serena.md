# Serena (oraios/serena)

Category: code intelligence MCP server (Python, language servers, local stdio) — functional peer

Dissected 2026-09-19 by fetching the live URLs below with `curl -sL`; every verbatim block was sliced from the fetched body by script, and each page's key quotes are re-checked in ../proposals/FACTCHECK.md. This project publishes NO llms.txt: it is here because it is one of XERJ's nearest functional peers (a local code-search or code-intelligence tool with an MCP server), and the 2026-09-18 set had none. The comparison notes are a reading, not a measurement.

## llms.txt

- URL: <https://raw.githubusercontent.com/oraios/serena/main/llms.txt> — HTTP 404
- Size: 0 bytes, 0 lines
- Install position: absent — no llms.txt at the repository root or at https://oraios.github.io/serena/llms.txt (both 404); no llms-install.md or SKILL.md. Install lives in the README's 'Quick Start'.
- Tone: n/a — there is no llms.txt. The README is the agent-install surface; see 'Other agent-facing files'.
- Section order: (no llms.txt is published)

### Install commands found

```sh
(README) uv tool install -p 3.13 serena-agent
(README) serena init
(docs, clients) claude mcp add --scope user serena -- serena start-mcp-server --context claude-code --project-from-cwd
(docs, clients) claude mcp add serena -- serena start-mcp-server --context claude-code --project "$(pwd)"
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

```text
claude mcp add --scope user serena -- serena start-mcp-server --context claude-code --project-from-cwd
```

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

````text
**Install Serena**. Serena is installed via uv as follows:

```bash
uv tool install -p 3.13 serena-agent
```

After successful installation, the command `serena` should be available in your shell.

**Initialise Serena**. To initialise Serena and verify that your setup works correctly, simply run:

```bash
serena init
```

By default, this will set up Serena to use the language server backend. To use the JetBrains backend instead, add the parameters `-b JetBrains` 
(see the [JetBrains Plugin documentation page](https://oraios.github.io/serena/02-usage/025_jetbrains_plugin.html) for additional usage details).  
Either way, you should receive a success message indicating that Serena has been initialised successfully.
````

## llms-full.txt

- URL: <https://oraios.github.io/serena/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Neither file exists.
- Install position: absent

## Other agent-facing files

### README.md — HTTP 200

<https://raw.githubusercontent.com/oraios/serena/main/README.md>

15,097 bytes. Quick Start is three bold-labelled steps: install with uv, `serena init` (which also verifies the setup), configure the client (a link). Near the top, a caution against installing from marketplaces.

```text
> Do not install Serena via an MCP or plugin marketplace! They contain outdated and suboptimal installation commands. 
> Instead, follow our [Quick Start](#quick-start) instructions.
```

### docs/02-usage/030_clients.md (per-client registration) — HTTP 200

<https://raw.githubusercontent.com/oraios/serena/main/docs/02-usage/030_clients.md>

29,001 bytes. One section per client; each launch command carries a `--context=<client>` flag so the server adapts its tool set, and either `--project-from-cwd` or an explicit `--project`. User-scope and project-scope variants are both printed for Claude Code.

```text
claude mcp add --scope user serena -- serena start-mcp-server --context claude-code --project-from-cwd
[...]
claude mcp add serena -- serena start-mcp-server --context claude-code --project "$(pwd)"
```

### AGENTS.md — HTTP 200

<https://raw.githubusercontent.com/oraios/serena/main/AGENTS.md>

436 bytes. For agents working on Serena itself; tells them to read a project memory first, with a fallback path when the tool is not available.

```text
This project uses Serena for development.
Make sure that you read the project's `critical_info` memory before working on it.
* When using Serena, the memory is provided either
  - along with the result of the `initial_instructions` tool, or
  - upon activating the project with `activate_project`.
  Be sure to always call the `initial_instructions` tool!
* When not using Serena, read `./.serena/memories/critical_info.md` manually.
```

### CLAUDE.md — HTTP 200

<https://raw.githubusercontent.com/oraios/serena/main/CLAUDE.md>

11 bytes: an include pointer.

```text
@AGENTS.md
```

## What they do better than XERJ

- The install step and the verify step are adjacent and named: 'To initialise Serena and verify that your setup works correctly, simply run: `serena init`' … 'you should receive a success message'. XERJ's `xerj init` does not verify anything (it does not check that a node answers).
- Both Claude Code scopes are printed, user scope first: `claude mcp add --scope user serena -- …`. A user-scoped stdio server is not written into the repository, which avoids committing a machine-specific absolute path.
- The server is told which client it is talking to (`--context claude-code`), so tool descriptions can fit the host.
- The project states where NOT to install from, and why ('They contain outdated and suboptimal installation commands'). A marketplace listing is a copy of install text that the project no longer controls.

## What XERJ does better

- XERJ publishes llms.txt, llms-full.txt and tool schemas; Serena's agent-readable surface is its README and its documentation site, which serves no llms.txt (404).
- One static binary with a checksum-verified installer; Serena needs `uv`, a Python toolchain and, per language, a language server.
- XERJ indexes documents and data files as well as code and serves an HTTP API.

## Adoptable ideas

- Recommend user scope for the Claude Code one-liner (`claude mcp add --scope user xerj -- "$(command -v xerj)" mcp`) and keep project scope for teams who agree on a path. RAN 2026-09-19: `xerj init` writes the running binary's absolute path into `.mcp.json`, the file Claude Code treats as shared with the team.
- Make `xerj init` verify as well as write (proposed — not implemented): check the node answers at `--url`, that the key is accepted, and print what is missing.
- Before listing XERJ in any marketplace or registry, decide who keeps that copy current; Serena's warning is what happens when nobody does.
