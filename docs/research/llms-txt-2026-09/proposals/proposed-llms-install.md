<!-- PROPOSAL — not a live file. https://xerj.org/llms-install.md is 404 today. report.md §9 lists what must exist before this ships. Delete this line when shipping. -->
# XERJ — install guide for AI agents

This guide is written for AI coding agents (Claude Code, Codex, Gemini CLI, Cursor, Cline, Windsurf, VS Code agents, OpenCode). Follow the sections in order. Run the commands yourself when you have a shell. Stop and ask the user only where a step says to. No shell? Go to [section 10](#10-no-shell).

Checked on 2026-09-19 against `xerj v1.0.0-rc.74` on Linux x86_64: every flag against `--help`, and sections 2b, 3, 4, 5, 6 (Claude Code, user and project scope; `xerj init`), 7 and 8 by running them. **The macOS and Windows commands follow the published installers and were not run in that check.** If this file and `xerj <subcommand> --help` disagree, `--help` wins.

What you will end up with: one `xerj` binary, a local node on `http://localhost:9200` with auth on, one indexed folder, an MCP server registered with the user's agent, and one search result you opened and confirmed.

## 0. Before you start

- XERJ is a local binary. It needs no account and no network after install. (Exception: `--embed-mode neural` downloads a ~90 MB model on first use.)
- **Search is lexical by default.** Do not tell the user they have neural or "by meaning" search unless you start the node with `--embed-mode neural` in section 3. The mode applies when documents are indexed, so choose before section 4.
- Tell the user what you are about to do and wait for a yes:

```text
Here is what I will do to set up XERJ:
1. Install the xerj binary to ~/.local/bin (the download is checked against its published SHA-256).
2. Start a local search node on 127.0.0.1:9200, with its data in ~/xerj-data and an API key it generates.
3. Index <folder>. I will show you a time estimate first.
4. Register the xerj MCP server with <your agent>.
Nothing is sent off this machine. Shall I proceed?
```

- If a node already answers on port 9200, or `~/xerj-data` already holds indices, **stop and ask**. Do not start a second node on the same data directory.

## 1. Check for an existing install

```sh
command -v xerj || ls "$HOME/.local/bin/xerj"
xerj --version                                   # or "$HOME/.local/bin/xerj" --version
curl -s -o /dev/null -w "%{http_code}\n" localhost:9200/_cluster/health    # 200 or 401 = a node is running; 000 = none
```

If `--version` prints `xerj v…`, skip section 2. If a node is running, skip section 3 and ask the user where its data directory is (you need the key in `<data-dir>/admin.key`).

## 2a. Install — one command

Linux and macOS (x86_64 and arm64):

```sh
curl -fsSL https://xerj.org/get | sh
export PATH="$HOME/.local/bin:$PATH"
xerj --version                                   # expect: xerj v1.0.0-rc.N
```

Windows 10+ (x64 and ARM64), PowerShell:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://xerj.org/get.ps1 | iex"
& "$env:LOCALAPPDATA\Programs\xerj\xerj.exe" --version
```

- The installer downloads the latest GitHub release for your OS and CPU, computes its SHA-256, compares it with the published digest, and **refuses to install** if the digest or a hashing tool is missing.
- Unix: it installs to `~/.local/bin` and does not edit your shell profile. In a fresh shell `xerj` may not be on `PATH`; use the `export` line above or the full path.
- Windows: it installs to `%LOCALAPPDATA%\Programs\xerj` and adds that to the user `PATH` (open a new terminal; opt out with `XERJ_NO_PATH=1`).
- Options: `XERJ_VERSION=vX.Y.Z` pins a release, `XERJ_INSTALL_DIR=<dir>` changes the location, `XERJ_LIBC=gnu` selects the glibc build on Linux (the default is static musl).

## 2b. Verified manual install (no `curl | sh`)

Use this when policy forbids piping a script into a shell. Set `target` to one of: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`. The block runs in a subshell and exits non-zero on any refusal.

```sh
(
  set -eu
  ver=$(curl -fsSL https://api.github.com/repos/xerj-org/xerj/releases/latest \
    | sed -n 's/.*"tag_name": *"v\([^"]*\)".*/\1/p')
  target=x86_64-unknown-linux-musl
  stage="xerj-${ver}-${target}"; asset="${stage}.tar.gz"
  base="https://github.com/xerj-org/xerj/releases/download/v${ver}"
  curl -fsSLO "$base/$asset"; curl -fsSLO "$base/$asset.sha256"
  want=$( { sha256sum "$asset" 2>/dev/null || shasum -a 256 "$asset"; } | cut -d ' ' -f 1 )
  printf %s "$want" | LC_ALL=C grep -qE '^[0-9a-f]{64}$' \
    || { echo "no working SHA-256 tool — refusing to install an unverified $asset" >&2; exit 1; }
  tr -d '\r' < "$asset.sha256" | LC_ALL=C grep -qxF -e "$want  $asset" -e "$want *$asset" \
    || { echo "CHECKSUM MISMATCH for $asset — do not install it" >&2; exit 1; }
  tar xzf "$asset"
  mkdir -p "$HOME/.local/bin" && install -m 0755 "$stage/xerj" "$HOME/.local/bin/xerj"
  "$HOME/.local/bin/xerj" --version
)
```

Expected: the last line is `xerj v1.0.0-rc.N` and the exit status is 0. Show the user the digest you computed and the line of the `.sha256` file it matched.

Do not replace the comparison with `sha256sum -c`: it reads the filename from the checksum file and can exit 0 without hashing the archive you downloaded. The reasoning is on https://xerj.org/docs/install.

Windows, PowerShell (`x86_64-pc-windows-msvc` or `aarch64-pc-windows-msvc`):

```powershell
$tag = (Invoke-RestMethod https://api.github.com/repos/xerj-org/xerj/releases/latest).tag_name
$target = 'x86_64-pc-windows-msvc'; $stage = "xerj-$($tag.TrimStart('v'))-$target"; $asset = "$stage.zip"
$base = "https://github.com/xerj-org/xerj/releases/download/$tag"
Invoke-WebRequest "$base/$asset" -OutFile $asset; Invoke-WebRequest "$base/$asset.sha256" -OutFile "$asset.sha256"
$want = ((Get-Content "$asset.sha256" -Raw) -split '\s+')[0].ToLower()
$got  = (Get-FileHash -Algorithm SHA256 $asset).Hash.ToLower()
if ($got -ne $want -or $want -notmatch '^[0-9a-f]{64}$') { throw "CHECKSUM MISMATCH for $asset - do not install it" }
Expand-Archive $asset -DestinationPath . -Force
$dir = "$env:LOCALAPPDATA\Programs\xerj"; New-Item -ItemType Directory -Force $dir | Out-Null
Move-Item -Force ".\$stage\xerj.exe" "$dir\xerj.exe"; & "$dir\xerj.exe" --version
```

macOS: a tarball downloaded with `curl` is not quarantined. One downloaded through a browser is; clear it once with `xattr -d com.apple.quarantine ./xerj`.

## 3. Start a node

Start it in the background and wait for it. A foreground start blocks your shell, and a request sent before the node is up gets `connection refused`. **Auth is on by default**: the node writes a key to `<data-dir>/admin.key` on first start, and a request without it gets HTTP 401 — so the wait loop must send the key.

```sh
nohup xerj --data-dir "$HOME/xerj-data" >"$HOME/xerj.log" 2>&1 &     # nohup: keeps running after this shell exits
until [ -s "$HOME/xerj-data/admin.key" ]; do sleep 0.3; done
export XERJ_API_KEY="$(cat "$HOME/xerj-data/admin.key")"
until curl -sf -H "Authorization: ApiKey $XERJ_API_KEY" localhost:9200/_cluster/health >/dev/null; do sleep 0.3; done
curl -s -H "Authorization: ApiKey $XERJ_API_KEY" localhost:9200/_cluster/health     # expect "status":"green"
```

```powershell
$xerj = "$env:LOCALAPPDATA\Programs\xerj\xerj.exe"; $data = "$env:USERPROFILE\xerj-data"
Start-Process $xerj -ArgumentList '--data-dir', $data -WindowStyle Hidden
while (-not (Test-Path "$data\admin.key")) { Start-Sleep -Milliseconds 300 }
$env:XERJ_API_KEY = (Get-Content "$data\admin.key" -Raw).Trim()
$h = @{ Authorization = "ApiKey $env:XERJ_API_KEY" }
do { Start-Sleep -Milliseconds 300; $ok = try { (Invoke-WebRequest -UseBasicParsing -Headers $h http://localhost:9200/_cluster/health).StatusCode -eq 200 } catch { $false } } until ($ok)
```

- Every `xerj` client command reads `XERJ_API_KEY`. HTTP clients send `Authorization: ApiKey <key>`. Never print the key, and never write it into a file that gets committed.
- If your shell tool starts a fresh process for every command, an `export` does not carry over. Put `XERJ_API_KEY="$(cat "$HOME/xerj-data/admin.key")"` in front of each later `xerj` command: `xerj search` and `xerj def` answer HTTP 401 without it. (`xerj autoindex` looks for a key file on its own — `./data/admin.key`, `./xerj-data/admin.key`, `~/.xerj/admin.key` and two others — and says which one it used; `~/xerj-data` is found only when you run it from your home directory.)
- `--insecure` turns off TLS **and** the key for every client on this machine: any local process can then read and write the index. It is for a throwaway loop. With it, drop the two key lines and the `-H` header above.
- Keep `--data-dir` **outside** any folder you will index. Otherwise the node indexes its own storage.
- Another port: `--port 9300` also claims 9301 (native REST) and 9302 (gRPC). `xerj autoindex` then needs `--url http://localhost:9300`; it ignores `XERJ_URL` on purpose. `xerj search` and `xerj def` read `XERJ_URL`.
- For neural embeddings add `--embed-mode neural` to the start command now. Switching later does not re-embed documents already indexed.
- Stop the node with SIGTERM (`kill <pid>`). Restarting with the same `--data-dir` keeps every index and the same key.

## 4. Index a folder

```sh
xerj autoindex <folder> --dry-run       # needs the node running; prints the plan and a measured estimate; indexes nothing
xerj autoindex <folder>
```

Expected last line on stderr: `xerj-done ok=true exit=0 reason=completed … files=<N> records=<N> … code_files_indexed=<N> code_files_junked=0`.

- Tell the user the estimate before the real run. The estimate is a floor for client-side extraction only — server indexing and embedding time are not in it. If it is more than a few minutes, ask first.
- Relay the `xerj-bar` progress line as it prints. If `pct` or `eta_s` is the literal word `unknown`, say so; do not invent a number.
- Exit codes: `0` complete · `3` complete with some files skipped as junk, or a catalog cleanup that could not run — **the corpus is indexed; read `reason`** · `4` a decision is needed: nothing was indexed; ask the user, then re-run with `--approve proceed|fast|cancel` · `2` usage error · `1` any other error: read the `error:` line first.
- `code_files` above 0 with `code_files_indexed=0` is a defect, not a success. Report it.
- Safe to re-run: the journal resumes and document ids are idempotent, so nothing is duplicated.
- Server memory grows with the number of documents indexed (an open defect). Index one large corpus at a time and restart the node between large corpora.

## 5. First query — this is the proof

```sh
xerj search "<something you know is in the folder, in plain words>"    # ─── <file>[:<line>] … then "N passage(s) from 'ax-*'"
xerj def "<a symbol name>"                                              # ─── <file>:<line>  [function] + its signature
```

Success is at least one hit you opened and confirmed. A green health check, a finished `--dry-run`, or `xerj --version` does not prove indexing worked. Hits are lexical matches unless the node runs `--embed-mode neural`.

Over HTTP the same check is `POST http://localhost:9200/ax-*/_search` with `{"query":{"match":{"body":"<words>"}},"fields":["_passage"],"_source":["ax_path"]}` and the `Authorization` header; success is `hits.total.value` greater than 0.

## 6. Register the MCP server

`xerj mcp` is a stdio MCP server inside the same binary. It does not start a node; the node from section 3 must be running. Every client needs three facts: the **absolute** path of the binary (an MCP host started from a desktop icon does not inherit your shell `PATH`), `XERJ_URL`, and `XERJ_AUTH` set to `ApiKey <key>`.

| Client | How |
| --- | --- |
| Claude Code | `claude mcp add --scope user xerj -e XERJ_URL=http://localhost:9200 -e XERJ_AUTH="ApiKey $XERJ_API_KEY" -- "$(command -v xerj)" mcp` · check: `claude mcp get xerj` → `Status: ✔ Connected` (its output prints the key; do not paste it) |
| Codex | `codex mcp add xerj --env XERJ_URL=http://localhost:9200 --env XERJ_AUTH="ApiKey $XERJ_API_KEY" -- "$(command -v xerj)" mcp` · file form: `[mcp_servers.xerj]` in `~/.codex/config.toml` (the key is `mcp_servers`, not `mcpServers`) |
| Gemini CLI | `gemini mcp add -s user -e XERJ_URL=http://localhost:9200 -e XERJ_AUTH="ApiKey $XERJ_API_KEY" xerj "$(command -v xerj)" mcp` · file form: `mcpServers` in `~/.gemini/settings.json` |
| Cursor | `.cursor/mcp.json` (project) or `~/.cursor/mcp.json` (global): the `mcpServers` block below |
| Windsurf | `~/.codeium/windsurf/mcp_config.json`: the `mcpServers` block below |
| Claude Desktop | `claude_desktop_config.json` (macOS `~/Library/Application Support/Claude/`, Windows `%APPDATA%\Claude\`): the `mcpServers` block below |
| Cline | MCP Servers → Installed → Advanced MCP Settings opens `cline_mcp_settings.json`: the `mcpServers` block below |
| VS Code | `.vscode/mcp.json` with root key **`servers`**: `{ "servers": { "xerj": { "type": "stdio", "command": "/ABSOLUTE/PATH/TO/xerj", "args": ["mcp"], "env": { … } } } }`. An `mcpServers` block copied from another client is silently ignored here. |
| OpenCode | `opencode.json` with root key **`mcp`** and an array command: `{ "mcp": { "xerj": { "type": "local", "command": ["/ABSOLUTE/PATH/TO/xerj", "mcp"], "environment": { … } } } }` |
| Zed | settings key `context_servers`; see Zed's MCP page for the current shape |

```json
{
  "mcpServers": {
    "xerj": {
      "command": "/ABSOLUTE/PATH/TO/xerj",
      "args": ["mcp"],
      "env": { "XERJ_URL": "http://localhost:9200", "XERJ_AUTH": "ApiKey <key>" }
    }
  }
}
```

- **Merge, do not replace.** Read the existing file first and add the `xerj` entry beside what is there.
- Windows: the command is `C:\\Users\\<you>\\AppData\\Local\\Programs\\xerj\\xerj.exe`; PowerShell prints it with `(Get-Command xerj).Source`. The `"$(command -v xerj)"` lines above are POSIX-shell only.
- **Scope, and the key.** `--scope user` keeps the entry and the key out of the repository. `--scope project` writes `.mcp.json`, which Claude Code treats as shared with the team: an absolute path there contains a user name and breaks for teammates, and a key there gets committed. If the team wants a shared entry, agree on the path, leave `XERJ_AUTH` out of the file, and have each person supply it through their own environment.
- `xerj init` (from the project root) writes `.mcp.json` and an `AGENTS.md` section, plus `.claude/skills/xerj/SKILL.md` if a `.claude/` directory exists in the project or in your home, and `.cursor/rules/xerj.mdc` if `.cursor/` exists. It merges, keeps other servers, and leaves `.mcp.json.bak` / `AGENTS.md.bak` for a file it edits. Three things it does today that you need to know: it writes the running binary's absolute path into `.mcp.json`; it does **not** write `XERJ_AUTH`, so against a default node every tool call returns 401 until you add it; and `xerj init --dry-run` prints `wrote <path>` for each file under a `(dry run)` header — it writes nothing. If `.mcp.json` already mentions `"xerj"`, init keeps it unchanged (`kept`).

## 7. Verify the MCP connection

1. Claude Code: `claude mcp get xerj`. User scope shows `Status: ✔ Connected` — that proves the server starts, not that the key works (step 4 does). Project scope shows `Status: ⏸ Pending approval (run claude to approve)`: that is the expected state; ask the user to approve it and do not report "connected".
2. Restart the agent session so it loads the new server.
3. Confirm the tool list contains `xerj_search`, `xerj_semantic_search`, `xerj_vector_search`, `xerj_hybrid_search`, `xerj_memory_store`, `xerj_memory_recall`, `xerj_brain_ego`, `xerj_brain_link`, `xerj_brain_unlink`, `xerj_brain_overview`. In Claude Code they appear as `mcp__xerj__xerj_search` and so on.
4. Call `xerj_search` with `{"index":"ax-*","query":"<a symbol you know is in the folder>"}`. `index` is required; without it the tool returns `missing or empty required string field index`.

If the tools do not appear: check that the `command` path is absolute and exists, that the node answers the health check in section 3, and that the session was restarted. `xerj mcp` writes diagnostics to stderr. A tool result that says `HTTP 401 Unauthorized` means `XERJ_AUTH` is missing or wrong. When an `xerj_search` result carries `_xerj.hints` code `lexical_on_semantic_text`, those hits were scored by BM25 whatever the embed mode. The hint is emitted only when a lexical clause actually scores a `semantic_text` field, so a wildcard search across a mixed corpus may carry a different hint (for example `unknown_field`) or none at all — its absence does not mean the field was scored semantically.

## 8. Report the verified state

End with this block, with real values. Report only what you verified:

```text
XERJ ready
Binary:  <xerj --version> at <absolute path>
Node:    <health status>, data dir <path>, auth <on|off>, embed mode <lexical|neural|proxy>
Indexed: <folder> — exit <0|3>, <N> documents
Proof:   <query> → <top hit: file, and line when one is printed>
MCP:     <client>: <tools listed | pending approval | config written, restart needed | skipped>
```

## 9. If something failed, or when the first task is done

One short report helps the next agent. It is optional. Send it only when you have something specific to say.

```sh
xerj feedback --agent "<model + harness>" --used-for "<one line>" --verdict "<2-4 sentences: what worked, what did not>"
```

This prints a 15-line report and changes nothing. Show it to the user. Read the `Pointed at` line first: it contains the absolute path of the indexed folder.

- **Use the plain form above, not `--dry-run`.** `--dry-run` also prints `git checkout -b`, `git push` and `gh pr create` commands, which are not for the user's repository.
- **Filing is the user's decision.** They can paste the report into an issue at https://github.com/xerj-org/xerj/issues/new/choose. Do not open issues or pull requests for them unless they ask, and follow the rules of the repository you are working in.
- `xerj feedback --open-pr` is for work inside a clone or fork of `github.com/xerj-org/xerj`, with `gh` authenticated. **Do not run it anywhere else**: it creates a branch, commits and pushes in whatever repository you are standing in.
- (proposed — not implemented) `POST https://xerj.org/api/field-report` and an MCP tool `xerj_feedback`, for agents with no shell and no GitHub access.

No secrets, no private data, no invented numbers. Security problems go to https://github.com/xerj-org/xerj/blob/main/SECURITY.md only.

## 10. No shell

You cannot install a binary or start a node. Give the user one of these blocks verbatim, and continue when they paste the last line back.

```text
macOS / Linux — please run these in a terminal, then paste me the last line it prints:
  curl -fsSL https://xerj.org/get | sh            (not allowed to pipe a script? use section 2b of https://xerj.org/llms-install.md)
  nohup ~/.local/bin/xerj --data-dir ~/xerj-data > ~/xerj.log 2>&1 &
  until [ -s ~/xerj-data/admin.key ]; do sleep 1; done; export XERJ_API_KEY="$(cat ~/xerj-data/admin.key)"
  until curl -sf -H "Authorization: ApiKey $XERJ_API_KEY" localhost:9200/_cluster/health >/dev/null; do sleep 1; done
  ~/.local/bin/xerj autoindex <the folder I should search>
  <the registration line for your client from section 6>        (MCP clients only; then restart this session)
  curl -s -H "Authorization: ApiKey $XERJ_API_KEY" localhost:9200/_cluster/health
```

```text
Windows — please run these in PowerShell, then paste me the last line it prints:
  powershell -ExecutionPolicy Bypass -c "irm https://xerj.org/get.ps1 | iex"      (then open a new PowerShell window)
  <the PowerShell block from section 3 of https://xerj.org/llms-install.md>
  xerj autoindex <the folder I should search>
  <the registration step for your client from section 6>         (MCP clients only; then restart this session)
  (Invoke-WebRequest -UseBasicParsing -Headers @{Authorization="ApiKey $env:XERJ_API_KEY"} http://localhost:9200/_cluster/health).Content
```

- MCP client: after the restart, continue at section 7.
- HTTP only, **on the same machine as the node**: continue with the HTTP check in section 5; the operations are listed under "HTTP only" in https://xerj.org/llms.txt. You need the key: ask the user for the contents of `~/xerj-data/admin.key`, and do not repeat it in your messages.
- A hosted assistant cannot reach the user's `localhost`. Give the user one request at a time as a `curl` command and work from the responses they paste back.
- `xerj autoindex` has no HTTP route and no MCP tool; only the operator can run it.

## 11. Undo

```sh
pgrep -a xerj                                     # find the node you started; check its --data-dir before you stop it
kill <pid of that node>                           # SIGTERM; never kill a node you did not start without asking
claude mcp remove xerj -s user                    # or -s project / -s local, matching how it was added
rm -f "$HOME/.local/bin/xerj"
rm -rf "$HOME/xerj-data" "$HOME/.xerj/autoindex"  # indices and the key; autoindex resume journals
```

- Ask before deleting the data directory. It holds every index the user built. Leave the rest of `~/.xerj` alone: `~/.xerj/brain` is the default data directory of `xerj brain`, a separate node.
- `xerj init` wrote `.mcp.json`, a `## XERJ (code search)` section in `AGENTS.md`, and possibly `.claude/skills/xerj/SKILL.md` and `.cursor/rules/xerj.mdc`. A file it printed as `wrote` did not exist before: delete it. A file it printed as `merged` or `appended` has a `.bak` (`.mcp.json.bak`, `AGENTS.md.bak`): restore it, or remove the `xerj` entry and that section by hand, then delete the `.bak`.
- Other clients: remove the `xerj` entry from the config file named in section 6.
- Windows (not run in this check): stop the `xerj` process you started, delete `%LOCALAPPDATA%\Programs\xerj` and `%USERPROFILE%\xerj-data`, and remove the install directory from the user `PATH`. The autoindex journal directory is `.xerj\autoindex` under `HOME`; where `HOME` is not set, the source falls back to `/tmp`, i.e. `\tmp\.xerj\autoindex` on the current drive (READ: `state.rs`, not run on Windows).

## Tools reference

- `xerj_search` — `index` (use `ax-*`) and a plain-string `query`; returns ranked passages, definitions first.
- `xerj_semantic_search` — `index`, `field`, `query` over a `semantic_text` field. Lexical unless the node runs `--embed-mode neural`.
- `xerj_vector_search` — kNN over a `dense_vector` field; bring your own `query_vector`.
- `xerj_hybrid_search` — fuse lexical and vector clauses; `fusion` is `rrf` or `linear`.
- `xerj_memory_store` / `xerj_memory_recall` — namespaced long-term memory. Recall is lexical unless the node runs `--embed-mode neural`.
- `xerj_brain_ego`, `xerj_brain_link`, `xerj_brain_unlink`, `xerj_brain_overview` — the knowledge-graph tools. `link`, `unlink` and `memory_store` write; ask before using them on the user's data.

Schemas: https://xerj.org/docs/agents/schemas/mcp-tools.json · Index of everything else: https://xerj.org/llms.txt
