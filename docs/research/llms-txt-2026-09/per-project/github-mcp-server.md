# GitHub MCP server (github/github-mcp-server)

Category: MCP server (Go binary, stdio + hosted remote) — stdio-binary peer

Dissected 2026-09-19 by fetching the live URLs below with `curl -sL`; every verbatim block was sliced from the fetched body by script, and each page's key quotes are re-checked in ../proposals/FACTCHECK.md. This project publishes NO llms.txt: it is here because it is one of XERJ's nearest functional peers (a local code-search or code-intelligence tool with an MCP server), and the 2026-09-18 set had none. The comparison notes are a reading, not a measurement.

## llms.txt

- URL: <https://raw.githubusercontent.com/github/github-mcp-server/main/llms.txt> — HTTP 404
- Size: 0 bytes, 0 lines
- Install position: absent — no llms.txt, llms-install.md, SKILL.md, AGENTS.md or CLAUDE.md at the repository root (all 404). Install lives in the README and in docs/installation-guides/.
- Tone: n/a — there is no llms.txt. The README is the agent-install surface; see 'Other agent-facing files'.
- Section order: (no llms.txt is published)

### Install commands found

```sh
(README) claude mcp add github -e GITHUB_PERSONAL_ACCESS_TOKEN=$GITHUB_PAT -- docker run -i --rm -e GITHUB_PERSONAL_ACCESS_TOKEN ghcr.io/github/github-mcp-server
(install-claude.md) claude mcp add --transport http github https://api.githubcopilot.com/mcp -H "Authorization: Bearer YOUR_GITHUB_PAT"
(install-gemini-cli.md) gemini extensions install https://github.com/github/github-mcp-server
(README, build from source) "command": "/path/to/github-mcp-server", "args": ["stdio"]
```

### Text written for an agent (verbatim)

_none_

### MCP snippet (verbatim)

````text
(from the README, 'Build from source' — the stdio-binary form closest to XERJ's:)
You should configure your server to use the built executable as its `command`. For example:

```JSON
{
  "mcp": {
    "servers": {
      "github": {
        "command": "/path/to/github-mcp-server",
        "args": ["stdio"],
        "env": {
          "GITHUB_PERSONAL_ACCESS_TOKEN": "<YOUR_TOKEN>"
        }
      }
    }
  }
}
```
````

### Feedback / contribution ask (verbatim)

_none_

### Best passage for driving install and first use (verbatim)

````text
### Verification
```bash
claude mcp list
claude mcp get github
```

### For Older Versions of Claude Code

If you're using Claude Code version **2.1.0 or earlier**, use this legacy command format:

```bash
claude mcp add --transport http github https://api.githubcopilot.com/mcp -H "Authorization: Bearer YOUR_GITHUB_PAT"
```

With an environment variable:
```bash
claude mcp add --transport http github https://api.githubcopilot.com/mcp -H "Authorization: Bearer $(grep GITHUB_PAT .env | cut -d '=' -f2)"
```

#### Windows (PowerShell)

If you see `missing required argument 'name'`, put the server name immediately after `claude mcp add`:

```powershell
$pat = "YOUR_GITHUB_PAT"
[...]
````

### One-click links

- `https://insiders.vscode.dev/redirect/mcp/install?name=github&config=<urlencoded JSON>  (README line 21, remote server; line 179, local Docker server)`

## llms-full.txt

- URL: <https://raw.githubusercontent.com/github/github-mcp-server/main/llms-full.txt> — HTTP 404
- Size: 0 bytes
- Relationship to llms.txt: Neither file exists.
- Install position: absent

## Other agent-facing files

### README.md — HTTP 200

<https://raw.githubusercontent.com/github/github-mcp-server/main/README.md>

113,197 bytes, 1,785 lines. Remote server first (one-click VS Code badge), local Docker server second (another badge), per-host guides linked, then 1,000 lines of tool reference. Token handling has its own subsection.

````text
### Token Security Best Practices

- **Minimum scopes**: Only grant necessary permissions
  - `repo` - Repository operations
  - `read:packages` - Docker image access
  - `read:org` - Organization team access
- **Separate tokens**: Use different PATs for different projects/environments
- **Regular rotation**: Update tokens periodically
- **Never commit**: Keep tokens out of version control
- **File permissions**: Restrict access to config files containing tokens

  ```bash
  chmod 600 ~/.your-app/config.json
  ```
````

### docs/installation-guides/install-claude.md — HTTP 200

<https://raw.githubusercontent.com/github/github-mcp-server/main/docs/installation-guides/install-claude.md>

12,705 bytes. Claude Code and Claude Desktop, remote and local, with shell AND PowerShell quoting for `claude mcp add-json`, a Verification section and a troubleshooting list that names log locations per OS.

````text
### Verification
```bash
claude mcp list
claude mcp get github
```

### For Older Versions of Claude Code

If you're using Claude Code version **2.1.0 or earlier**, use this legacy command format:

```bash
claude mcp add --transport http github https://api.githubcopilot.com/mcp -H "Authorization: Bearer YOUR_GITHUB_PAT"
```

With an environment variable:
```bash
[...]
````

### docs/installation-guides/install-opencode.md — HTTP 200

<https://raw.githubusercontent.com/github/github-mcp-server/main/docs/installation-guides/install-opencode.md>

7,578 bytes. States OpenCode's root key and its env-interpolation syntax.

```text
[OpenCode](https://opencode.ai) is a terminal-based AI coding agent that exposes MCP servers under the `mcp` key in `opencode.json` (or `opencode.jsonc`).
```

### docs/installation-guides/install-gemini-cli.md — HTTP 200

<https://raw.githubusercontent.com/github/github-mcp-server/main/docs/installation-guides/install-gemini-cli.md>

6,229 bytes. Global and project settings paths; an extension install as the recommended route.

```text
MCP servers for Gemini CLI are configured in its settings JSON under an `mcpServers` key.

- **Global configuration**: `~/.gemini/settings.json` where `~` is your home directory
- **Project-specific**: `.gemini/settings.json` in your project directory
```

### server.json (MCP Registry manifest) — HTTP 200

<https://raw.githubusercontent.com/github/github-mcp-server/main/server.json>

1,844 bytes. Registry name `io.github.github/github-mcp-server`, one `oci` package with stdio transport and typed runtime arguments (a secret is marked `isSecret`), and one streamable-http remote.

```text
"packages": [
    {
      "registryType": "oci",
      "identifier": "ghcr.io/github/github-mcp-server:${VERSION}",
      "transport": {
        "type": "stdio"
      },
```

## What they do better than XERJ

- One guide per host — Copilot CLI, other Copilot IDEs, Claude, Codex, Cursor, Gemini CLI, OpenCode, Windsurf, Zed, Rovo Dev — each with that host's file, key and quirks. XERJ documents one `mcpServers` block.
- The Claude guide gives the `add-json` line three times: POSIX with a literal token, POSIX with an environment variable, and PowerShell with its own quoting. XERJ's MCP text is POSIX-only.
- A Verification section (`claude mcp list`, `claude mcp get github`) and a troubleshooting list that says where each client's MCP logs are.
- A registry manifest in the repository root, so the server is installable from the MCP Registry and from clients that read it.
- Secrets guidance sits beside the config that needs a secret: environment variable, .gitignore, `chmod 600`, and the honest note that 'Some applications (like Windsurf) require hardcoded tokens in config files.'

## What XERJ does better

- XERJ publishes llms.txt and llms-full.txt; this project publishes neither and its README is 113 KB, most of it tool reference.
- XERJ's MCP server is the same binary and needs no container runtime. It does need a credential: a default node is auth-on, so `xerj mcp` requires `XERJ_AUTH` (`ApiKey <key>` from `<data-dir>/admin.key`) and returns HTTP 401 without it.
- XERJ warns that a desktop-launched MCP host does not inherit the shell PATH and tells the agent to use an absolute path; this README's stdio example uses a placeholder path without saying why.

## Adoptable ideas

- Add `server.json` for the MCP Registry (proposed — not implemented). The registry documents an `mcpb` package type for prebuilt binaries from GitHub Releases, which is what XERJ already publishes.
- Give Windows users the PowerShell form of every registration line, as this guide does, or label the line POSIX-only.
- Put the secret-handling rules next to the `XERJ_AUTH` line: the key goes in the client's env block or an environment variable, never in a prompt, a log or a committed `.mcp.json`.
- Add Gemini CLI and OpenCode rows to the per-client table, with OpenCode's different root key (`mcp`) and array-valued `command`.
