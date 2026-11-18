# XERJ for Herdr

This plugin connects Herdr worktrees to XERJ. When Herdr creates a worktree, the `worktree.created` hook runs `xerj init` from that checkout. This lets XERJ generate project-local agent configuration such as `.mcp.json`. The **Index this workspace** pane action is separate: it runs `xerj autoindex .` from the focused pane's workspace.

The manifest is currently Linux-scoped because the entrypoints use Bash. The plugin requires Herdr 0.7.0 or newer and an installed `xerj` executable available on `PATH`.

## Install

From a Herdr session, install the plugin from the repository checkout or a published GitHub subdirectory:

```sh
herdr plugin install xerj-org/xerj/integrations/herdr
```

For local development, link the directory instead:

```sh
herdr plugin link ./integrations/herdr
```

The plugin does not install XERJ. Install XERJ separately and confirm it is available with `command -v xerj`.

## Verify

Create a new Herdr worktree and inspect its checkout. If a supported project agent surface is present, the hook should create or update `.mcp.json` there:

```sh
herdr worktree create --cwd /absolute/path/to/repository --branch herdr-test
cat /absolute/path/to/worktree/.mcp.json
```

The plugin action can be invoked from a focused pane through Herdr's action UI, or directly while testing:

```sh
herdr plugin action invoke xerj.herdr.index-workspace
```

The action indexes the pane's workspace with `xerj autoindex .`. Check recent plugin logs if a command fails:

```sh
herdr plugin log list --plugin xerj.herdr
```

Restart the coding-agent session after initialization. A successful MCP setup is confirmed when `xerj_search` appears in the agent's tool list and a search call returns a result. Tested end-to-end with Herdr 0.9.5 and Claude Code 2.1.316 against XERJ v1.0.0-rc.100 on Linux (WSL2), confirming worktree initialization, `.mcp.json` generation, pane action workspace indexing, and `xerj_search` tool invocation.

## Remove

Disable the plugin without removing its files:

```sh
herdr plugin disable xerj.herdr
```

Unregister it completely:

```sh
herdr plugin uninstall xerj.herdr
```

For a locally linked plugin, use `herdr plugin unlink xerj.herdr`. These commands unregister the plugin but leave the repository checkout in place. Files written by `xerj init`, including `.mcp.json`, remain under the worktree and should be removed or edited according to the project's normal configuration policy.

## Without Herdr

Herdr is optional. To configure the current project directly, run:

```sh
cd /absolute/path/to/project
xerj init
xerj autoindex .
```

`xerj init` is the standalone XERJ setup command: it detects supported agent surfaces and writes the smallest project-local configuration it needs. The plugin only automates that command for newly created Herdr worktrees and adds a manual indexing action.
