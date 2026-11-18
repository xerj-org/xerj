# Advisory search hints for Claude Code and Gemini CLI

Opt in from the project directory (macOS/Linux):

```sh
xerj init --hooks --dry-run
xerj init --hooks
```

Success: init prints `enabled Claude Code search hints`. Review the new
`PreToolUse` group in `.claude/settings.json`, then restart Claude Code. Ordinary
init does not install it. This feature is separate from `xerj init --memory`;
configure/remove the two features in separate invocations.

For Gemini CLI, add `--host gemini` to every invocation:

```sh
xerj init --hooks --host gemini --dry-run
xerj init --hooks --host gemini
```

That writes an `AfterTool` group to `.gemini/settings.json` and keeps its private
state in `.gemini/xerj-hints/`. `--host` accepts `claude` (the default) or
`gemini`, may be given once, and requires `--hooks`. The two hosts are
independent: installing or removing one never reads or rewrites the other's
settings, backups or counters, and a project may run both at once.

Gemini differences, all host-imposed. The hint is delivered on `AfterTool`,
because `BeforeTool` has no field that reaches the model — only `AfterTool`
appends `hookSpecificOutput.additionalContext` to the tool result, wrapped in
`<hook_context>…</hook_context>`. The installed `timeout` is `2000`, since
Gemini counts milliseconds where Claude counts seconds; the HTTP budget inside
the hook is identical for both hosts. Matched tools are `grep_search`, its alias
`search_file_content`, `glob` and `read_file`. The matcher is anchored
(`^(…)$`) because Gemini applies it unanchored, which would otherwise catch MCP
tools named `mcp_<server>_grep_search`. Gemini also HTML-escapes the hint's
angle brackets, so the delivered placeholder reads `&lt;words&gt;`.

Gemini reads project settings, and therefore this hook, only in a directory it
trusts; an untrusted directory refuses to start before any hook runs. Setup is
refused outright when `hooksConfig.enabled` is `false` in the merged settings,
because the host would silently ignore the installed group.

Index this same folder under a prefix you own, using the estimate/approval
workflow in AGENTS.md. The hook only suggests searches when its configured
loopback node's catalog records a nonempty run for this exact root.
The suggestion includes the prefix and shell-quoted node URL; it does not search, index or start a node.
Subdirectories and additional directories abstain rather than guess the scope.

On `Grep`, `Glob`, or the third `Read` in a session, the hook may return one
line of `PreToolUse.additionalContext`. It never returns a permission decision
or replacement input; the host retains its normal permission flow and tool call.
After a hint, at least ten matching tool invocations must pass before another.
Calls made while the node is offline count but do not consume a hint slot.
Counters survive hook process exits. A bounded FIFO keeps 64 session hashes;
an evicted session starts a new counter if used again. Shell grep/rg is not
intercepted. Quotas, counters and session hashes are per host: a Gemini hint
never consumes a Claude hint slot. Windows and remote nodes are unsupported in
this version.

`--url` or `XERJ_URL` at setup selects the loopback node. Only the URL is saved.
Runtime `XERJ_AUTH`, then `XERJ_API_KEY`, then origin-checked local admin-key
lookup supplies auth. No key is stored. Proxies and redirects are disabled.
An eligible invocation makes one read-only catalog search with a 100 ms connect
and 300 ms request timeout and a 256 KiB response cap; the installed command
has a two-second host timeout. Startup, key discovery and filesystem work are
outside the HTTP timeout; no laptop-wide latency claim is made.

There is no hint on a failed/partial/truncated catalog read, over 200 run
records, ambiguous latest root ownership, unsupported prefix spelling, wrong
host event/cwd, corrupt state, busy lock or offline/unauthorized node. Prefixes
are lowercase ASCII letters/digits, hyphens/underscores, at most 128 bytes.
The catalog is evidence of a prior run, not a freshness check against files or
manually deleted indices. Tool input, search patterns, transcript paths and raw
session IDs are neither stored nor sent. Only catalog metadata and runtime
authentication go to the node; local counters retain SHA-256 session hashes.

## Remove, recover, upgrade

```sh
xerj init --hooks --remove --dry-run
xerj init --hooks --remove
```

Removing the Gemini host takes the same `--host gemini` it was installed with:
`xerj init --hooks --host gemini --remove`. Everything below applies to both
hosts, reading `.gemini/settings.json`, its `AfterTool` array and
`.gemini/xerj-hints/` for Gemini.

Success: the uniquely owned group is removed; foreign groups/settings and
session memory remain. Config is disabled before settings publication; existing
backups/counters remain in private `.claude/xerj-hints/`. After removal that
folder may be deleted manually; it contains no indexed documents. Installing
again enables the hook. Re-run init after moving/upgrading the binary: the old
owned group is replaced with the new absolute, shell-quoted executable path.

Malformed settings, edited/duplicated/missing managed groups and unowned exact
matches refuse setup/removal without rewriting settings. Restore just the owned
`group` from `.claude/xerj-hints/config.json` into the PreToolUse array (once),
then retry removal. If settings were copied without config.json, manually remove
only the reviewed obsolete group before reinstalling; init never adopts it. Backups `settings-<digest>.bak` contain exact prior settings;
compare them before manually restoring to preserve intervening user edits.
A busy lock means retry when the other installer/hook finishes. Corrupt counters
can be moved aside to reset rate limits. Setup refuses symlinks and requires the
private state directory to be owned by the current user with mode 0700.

Settings/config are individually atomically replaced, not a multi-file crash
transaction. A settings publication error rolls config back; a power loss
between publications may require the manual owned-group recovery above. The
file checks defend ordinary misconfiguration/symlinks, not a hostile same-user
process racing every filesystem operation. Settings are project-controlled
executable hook configuration: enable only in a trusted workspace.

## Reproduce

```sh
python3 tools/search-hints/smoke.py --binary engine/target/release/xerj
python3 tools/search-hints/smoke.py --binary engine/target/release/xerj --authenticated
```

Both commands exercise both hosts against one private node, including their
coexistence and per-host quotas.

For the optional actual-host controlled sample, add `--claude-sample --receipt NEW.json`
(requires an already authenticated Claude Code CLI; sends the synthetic task to
your configured model). It disables host session persistence, limits each run
to four turns, and deliberately asks the agent to follow a delivered hint.

The default smoke commands use only a private node and a tiny synthetic Rust project; they do not
access existing indices. See VERIFICATION.md for executed results, a controlled
before/after sample and the explicit actual-host and platform limits.

Host schema references:
[Claude Code hooks](https://code.claude.com/docs/en/hooks#pretooluse-decision-control)
and [Gemini CLI hooks](https://geminicli.com/docs/hooks/reference/).

Implementation evidence: existing project-owned lifecycle hook configuration in
`engine/crates/xerj-autoindex/src/session_memory.rs:190` and catalog cohort
ownership in `engine/crates/xerj-autoindex/src/catalog.rs:620`. Nonblocking file
locking uses fs2 0.4.3 `src/unix.rs:38-39` (`LOCK_EX | LOCK_NB`, licensed
MIT/Apache-2.0; inspected in the local Cargo source). No peer implementation is
copied. Each host's JSON format comes from that host's official reference. The
`AfterTool`-only delivery claim was read out of the Gemini CLI 0.66.0 bundle
(`executeToolWithHooks` in `bundle/chunk-RZ3ZVTZB.js:344483-344510`, which
appends `additionalContext` to the tool result and has no equivalent on
`BeforeTool`) and then confirmed against the running host; see VERIFICATION.md.
