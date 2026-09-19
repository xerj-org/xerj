# What was run against the XERJ binary, 2026-09-19

Every command the proposals print was checked against a binary, not against memory. This file is the
record: the command, the literal result, and what it changes in the proposals. Paths under the tester's
home directory are written `$HOME`; API keys are never shown.

- Binary: `xerj v1.0.0-rc.74`, the latest GitHub release on the day (published 2026-09-08), target
  `x86_64-unknown-linux-musl`, installed by running the verified manual block below. Linux x86_64 only.
  **Nothing here was run on macOS or Windows.**
- Source cross-check: `git diff v1.0.0-rc.74..52d1d7a8` touches none of `init.rs`, `feedback.rs`, the CLI
  argument parsing or the server's flags. One MCP tool schema changed on `main` after the release
  (`xerj_memory_recall` gained `hybrid` and `fusion`); it is not in the released binary.
- Node: one throwaway node at a time, auth ON (the default), a scratch data directory. Two separate passes:
  pass 1 on ports 10960–10962 (sections 1–10), pass 2 on ports 12960–12962 (section 11), each by a
  different agent session, each starting from a fresh download. Every node was stopped by PID afterwards.
- Client: `claude` 2.1.277 for the one Claude Code check.

## 1. Verified manual install (no `curl | sh`) — works as printed

The block in `proposed-llms-install.md` §2b was run verbatim with `HOME` pointed at a scratch directory.

```text
exit=0
xerj v1.0.0-rc.74
archive SHA-256 (matched the published .sha256 line):
2b67fdb1a181759bb491fce0f4894c44d2ed1aea78ac5b3481ec1430b23a6e3b  xerj-1.0.0-rc.74-x86_64-unknown-linux-musl.tar.gz
```

`curl -fsSL https://xerj.org/get | sh` was **not** run (it installs into the real `~/.local/bin`).

## 2. Boot with auth on, and the wait loop that works

```text
$ xerj --port 10960 --data-dir "$HOME/xerj-data" >"$HOME/xerj.log" 2>&1 &
$ curl -s -o /dev/null -w "%{http_code}\n" localhost:10960/_cluster/health          # no key
401
$ until [ -s "$HOME/xerj-data/admin.key" ]; do sleep 0.3; done
$ export XERJ_API_KEY="$(cat "$HOME/xerj-data/admin.key")"
$ until curl -sf -H "Authorization: ApiKey $XERJ_API_KEY" localhost:10960/_cluster/health >/dev/null; do sleep 0.3; done
$ curl -s -H "Authorization: ApiKey $XERJ_API_KEY" localhost:10960/_cluster/health | grep -o '"status":"[a-z]*"'
"status":"green"
```

**Consequence.** The 2026-09-18 draft's wait loop, `until curl -sf localhost:9200/_cluster/health`, never
exits against a default node: health answers 401 without the key and `-f` treats that as failure. The
draft avoided this by making `--insecure` the only boot path. The proposals now lead with the default
(auth on) and send the key in the loop.

## 3. `xerj autoindex`

```text
$ xerj autoindex <folder> --url http://localhost:10979 --dry-run        # nothing listens there
exit=1
error: endpoint unreachable: http://localhost:10979: … Connection refused (os error 111)

$ xerj autoindex <folder> --url http://localhost:10960 --dry-run        # XERJ_API_KEY exported
xerj-done ok=true exit=0 reason=dry-run wall=0.7s files=5 …
$ xerj autoindex <folder> --url http://localhost:10960
xerj-done ok=true exit=0 reason=completed wall=1.5s files=5 records=134 datasets=1 junk_files=0 code_files=4 code_files_indexed=4 code_files_junked=0
```

- `--dry-run` needs a reachable node (exit 1 without one).
- `autoindex` ignores `XERJ_URL` on purpose (source: `crates/xerj-autoindex/src/cli.rs`, "a stale var must
  not redirect a write"); a node on a non-default port needs `--url`. `xerj search`, `xerj def` and
  `xerj gain` DO read `XERJ_URL`.
- With no `--api-key` and no `XERJ_API_KEY`, `autoindex` looks for a key file and says so on stderr:
  `autoindex: no --api-key/XERJ_API_KEY given; using the admin key at ./data/admin.key`. The candidates
  are `./data/admin.key`, `./xerj-data/admin.key`, `/var/lib/xerj/admin.key`, `~/.xerj/brain/admin.key`,
  `~/.xerj/admin.key`, loopback URLs only (READ, `cli.rs`). `xerj search` and `xerj def` do not do this:

```text
$ xerj search "…"                                     # no key, cwd without ./data
xerj search: no XERJ node reachable at http://localhost:10960 (the server … requires authentication and no API key was supplied (HTTP 401).
  To fix, in order of preference:
  1. export XERJ_API_KEY="$(cat <data_dir>/admin.key)" …
```

## 4. `xerj search`, `xerj def`

```text
$ xerj def merged_mcp_json -k 1
─── init.rs:95  [function]
    fn merged_mcp_json(existing: Option<&str>, url: &str) -> Result<String, String>
1 definition(s) from 'ax-*'. Cite path:line for anything you rely on.

$ xerj search "merge the xerj server into .mcp.json" -k 2 --full 200
─── init.rs  (score 7.81)
    … /// Merge the xerj server into an `.mcp.json`, preserving everything else.
─── mcp-src/lib.rs:358  [function]  (score 7.06)
```

A `search` hit is `file:line` for a definition and `file` alone for a prose passage, so the completion
block asks for "file, and line when one is printed".

## 5. The HTTP-only requests — all answered as the proposal says

| Request | Result |
|---|---|
| `POST /ax-*/_search` with `{"query":{"match":{"body":"…"}},"fields":["_passage"],"_source":["ax_path"]}` | 200; `hits.total.value` 4; each hit has `_source.ax_path` and `fields._passage[0].text` |
| `PUT /<index>` with a `semantic_text` mapping | `{"acknowledged":true,…}` |
| `PUT /<index>/_doc/1?refresh=true` | `"result":"created"` |
| `{"query":{"semantic":{"field":"body","query":"…"}}}` | 200, 1 hit |
| `POST /_memory/<ns>` with `{"text":"…"}` | `{"id":"…","namespace":"…","created":true}` |
| `POST /_memory/<ns>/_recall` with `{"query":"…","k":5}` and with `"semantic":true` | 200, 1 hit each |
| hybrid with `"fusion":"rrf"` (each entry wrapped as `{"query":{…}}`) | 200 |
| hybrid with `"fusion":"learned"` | 400, `hybrid fusion learned is not yet supported; use rrf or linear` |

Response size, same query, 5 hits, a 5-file / 109 KB corpus: default `_search` **133,264 bytes**;
with `"fields":["_passage"]` and `"_source":["ax_path"]` **8,520 bytes**. One run on a toy corpus — it
shows the direction, not a number to publish. The figures in today's llms.txt (416,630 and 12,122 bytes)
have no run reference in the repository and are not carried into the proposal.

## 6. `xerj mcp` over stdio

```text
initialize → serverInfo {'name': 'xerj-mcp', 'version': '1.0.0-rc.74'}, protocol 2024-11-05
tools/list → 10 tools: xerj_search, xerj_semantic_search, xerj_vector_search, xerj_hybrid_search,
             xerj_memory_store, xerj_memory_recall, xerj_brain_ego, xerj_brain_link, xerj_brain_unlink, xerj_brain_overview
tools/call xerj_search {"query":"merged_mcp_json"}                         → isError: true, "missing or empty required string field `index`"
tools/call xerj_search {"index":"ax-*","query":"merged_mcp_json","size":1} → isError: false (XERJ_AUTH="ApiKey <key>" set)
same call, XERJ_AUTH unset, auth-on node                                  → isError: true, "XERJ returned HTTP 401 Unauthorized: …"
```

**Consequences.** (a) `xerj_search` needs `index`; the proposals say `"index":"ax-*"`. (b) The response
carries its own correction — `_xerj.hints[0].code = "lexical_on_semantic_text"` — which the proposals
point agents at. (c) **`xerj init` writes an MCP entry with `XERJ_URL` only, so against a default
(auth-on) node every tool call returns 401 until `XERJ_AUTH` is added by hand.** `xerj init` has no flag
for it.

## 7. `xerj init`

```text
$ xerj init --dry-run          # empty directory; a ~/.claude directory existed in HOME
xerj init (dry run) — wiring XERJ into this project:
  wrote    …/.mcp.json
  wrote    …/.claude/skills/xerj/SKILL.md
  wrote    …/AGENTS.md
exit=0          files on disk afterwards: 0
```

The skill line appears only when `.claude/` exists in the project or in `$HOME` (READ: `init.rs`,
`claude_dir_present`; RAN in pass 2, section 11, where neither existed and no skill was written).

`--dry-run` prints `wrote` for files it did not write (READ: `init.rs` calls the same `report("wrote", …)`
in both modes). The real run writes the three files; `.mcp.json` holds
`"command": "<absolute path of the running binary>"` (READ: `std::env::current_exe()`), i.e. a path
containing the user's home directory, in the file Claude Code treats as shared with the team. The skill's
front matter is `description: Definition-first code search over locally indexed repos (xerj search / xerj def)`
— what it does, with no "use when…" trigger.

## 8. `xerj feedback`

```text
$ xerj feedback --no-autofill --agent … --used-for … --verdict …     → exit 0, 15 lines on stdout, 0 on stderr, 0 files written
$ xerj feedback --no-autofill --dry-run   (same flags)              → exit 0, 29 lines: the report PLUS
      git checkout -b field-report/<slug> · git add … · git commit --only … · git push -u origin … · gh pr create --base main …
```

The plain form is the one that "drafts and changes nothing". `--dry-run` prints the git and `gh`
commands an agent is then likely to paste to its user or run. Its replay command also omits `--agent`.
With a node running, the auto-filled line is
`**Pointed at:** 134 records across 1 dataset(s) under <ABSOLUTE PATH OF THE INDEXED FOLDER> (autoindex run …)` —
an absolute local path, which a public pull request would publish.

`--open-pr`, run in a throwaway repository that is NOT xerj (local bare `origin`, a stub `gh` that logs
its arguments and exits 0):

```text
before: branch=main commits=1 remote_branches=1
exit=0
after:  branch=field-report/foreign-repo-test commits=2
origin: field-report/foreign-repo-test, main
gh stub log: gh --version · gh auth status · gh pr create --base main --head field-report/foreign-repo-test --title … --body …
```

It created a branch, committed `user-feedback/16-agent-field-reports/2026-09-19-foreign-repo-test.md`,
pushed to that repository's `origin`, called `gh pr create` with no `--repo`, and left the working tree
on the new branch. The only repository check is `git rev-parse --is-inside-work-tree` (READ:
`feedback.rs`, `in_git_repo()`); the error text says "must run inside a checkout of the xerj repository"
but nothing tests for it. Today's llms.txt calls this command "required, one command".

## 9. Claude Code registration

```text
$ claude mcp add --scope project xerj -e XERJ_URL=http://localhost:9200 -- "<abs path>/xerj" mcp
Added stdio MCP server xerj with command: <abs path>/xerj mcp to project config
$ claude mcp get xerj
  Scope: Project config (shared via .mcp.json)
  Status: ⏸ Pending approval (run `claude` to approve)
```

So the verify line for a project-scoped add is "expect *Pending approval*, then ask the user to approve",
not "listed as connected". `--scope user` was run in pass 2 with `HOME` pointed at a scratch directory (section 11); `--scope local` was
not run.

## Not run

`curl … | sh`; anything on macOS or Windows; `--embed-mode neural`; `codex`, `gemini`, Cursor, VS Code,
Windsurf, Cline, OpenCode, Zed and Claude Desktop registration (each line in the client table is quoted
from that client's documentation or from a third-party guide, with its FACTCHECK id); the Cursor deep link.
`claude mcp add --scope local` (only user and project scope were run).

## 10. Restart on the same data directory

```text
stop the node (SIGTERM) → start it again with the same --data-dir
admin.key SHA-256 before == after: yes        GET /ax-*/_count → {"count":134,…}
```

Indices and the key survive a restart, as the install guide says.

## 11. Pass 2 — an independent re-run of the proposals as revised

A second agent session repeated the checks the proposals depend on, from a fresh download, on ports
12960–12962 (`--port 12960`), with `HOME` pointed at a scratch directory. The corpus was four files: three
Rust sources from the repository and a two-line Markdown note.

```text
section 2b block, verbatim             exit=0, xerj v1.0.0-rc.74, archive SHA-256 2b67fdb1…6e3b = the .sha256 line
nohup xerj --port 12960 --data-dir …   no key: 401 · with key: "status":"green"
xerj autoindex <folder> --dry-run      exit=0 · "estimate: at least 0.0 s–0.0 s — a MEASURED FLOOR for client-side
                                       extraction, not a prediction of the whole run: server indexing, embedding and
                                       network time are not in it"
xerj autoindex <folder>                exit=0 · xerj-done ok=true exit=0 reason=completed … files=4 records=131
                                       code_files=3 code_files_indexed=3 code_files_junked=0
xerj search "…" -k 2                   ─── mcp_lib.rs:358 [function] … / "2 passage(s) from 'ax-*'. Cite ax_path:line …"
xerj def merged_mcp_json -k 1          ─── init.rs:95 [function] fn merged_mcp_json(…)
xerj search "…" --json                 raw response; hits.total.value = 8
xerj autoindex map --json              keys run, datasets, correlations, junk_files, duplicate_files, gotchas; each
                                       dataset carries fields_json (with "examples") and sample_queries_json
xerj autoindex ./ref --prefix ref      exit=0; xerj search --prefix ref … → "─── projA/init.rs" … "from 'ref-*'"
```

HTTP-only section, every request as printed (with the key): health `"status":"green"`; `POST /ax-*/_search` with
`fields:["_passage"]` and `_source:["ax_path"]` → `hits.total.value` 3, `_source.ax_path` and
`fields._passage[0].text` present; `PUT /notes` → `"acknowledged":true`; `PUT /notes/_doc/1` and
`POST /notes/_doc` → `"result":"created"`; `POST /_bulk` → `"errors":false`; a `semantic` query → 200;
`POST /_memory/demo` → `"created":true`; `_recall` with and without `"semantic":true` → 1 hit each.

**Correction this pass made.** The `semantic` query's response carried **no** `_xerj` block. The hint
`lexical_on_semantic_text` is emitted when a *lexical* clause (`match`, and so the `xerj_search` tool) scores a
`semantic_text` field with BM25 (READ: `es_compat.rs`, `lexical_on_semantic_text_hint`, which returns nothing
when the query reached the vector). It does not report the embed mode. The proposals now say exactly that.

MCP over stdio, `XERJ_URL` + `XERJ_AUTH` set: `initialize` → `xerj-mcp` 1.0.0-rc.74; `tools/list` → 10 tools;
`xerj_search` `required` = `["index"]`; without `index` → isError, "missing or empty required string field
`index`"; with `"index":"ax-*"` → isError false, text begins `{"_xerj":{"hints":[{"code":"lexical_on_semantic_text"…`;
same call without `XERJ_AUTH` → isError, "XERJ returned HTTP 401 Unauthorized".

`xerj feedback` (flags as in section 8): plain form exit 0, 15 lines on stdout, 0 on stderr, 0 files; the
`Pointed at` line holds the absolute corpus path. `--dry-run`: 29 lines, including `git checkout -b`,
`git push -u origin` and `gh pr create`. `--open-pr --no-autofill` in a throwaway non-XERJ repository with a
local bare `origin` and a stub `gh`: exit 0; branch `field-report/foreign-repo-test`; 1 → 2 commits; the branch
pushed to that `origin`; the stub logged `gh --version`, `gh auth status`, `gh pr create --base main --head …`.
Section 8 reproduced.

`xerj init`, `HOME` without `.claude/`:

```text
empty directory, --dry-run             "xerj init (dry run) …" · wrote …/.mcp.json · wrote …/AGENTS.md · 0 files after
directory with .cursor/, an .mcp.json  merged …/.mcp.json · wrote …/.cursor/rules/xerj.mdc · appended …/AGENTS.md
holding another server, an AGENTS.md   files: .mcp.json, .mcp.json.bak, AGENTS.md, AGENTS.md.bak, .cursor/rules/xerj.mdc
                                       .mcp.json keeps "other" and adds "xerj" with the absolute binary path and
                                       XERJ_URL only; AGENTS.md gains "## XERJ (code search)"; no SKILL.md written
```

Claude Code 2.1.277, `HOME` pointed at a scratch directory:

```text
claude mcp add --scope project xerj -e XERJ_URL=… -- <abs path> mcp   → .mcp.json gains "type": "stdio" + the path
claude mcp get xerj                                                   → Status: ⏸ Pending approval (run `claude` to approve)
claude mcp add --scope user xerj -e XERJ_URL=… -e XERJ_AUTH="ApiKey $XERJ_API_KEY" -- "$(command -v xerj)" mcp
claude mcp get xerj                                                   → Scope: User config · Status: ✔ Connected
                                                                        (the Environment lines print XERJ_AUTH in full)
claude mcp list                                                       → xerj: … - ✔ Connected
claude mcp remove xerj -s user                                        → Removed MCP server xerj from user config
```

"Connected" is the MCP handshake. `xerj mcp` completes `initialize` and `tools/list` with no key at all (the
no-`XERJ_AUTH` MCP run above), so a missing or wrong key shows up only on the first tool call — which is why the
proposals verify with an `xerj_search` call.

Also READ in pass 2 (source at `8837a119`, not run): the installer options (`XERJ_VERSION`, `XERJ_INSTALL_DIR`,
`XERJ_LIBC`; `XERJ_NO_PATH` and the `%LOCALAPPDATA%\Programs\xerj` default in `get.ps1`); the release asset names
of v1.0.0-rc.74, including the Windows `.zip` whose single directory holds `xerj.exe`; the autoindex state-dir
default `$HOME/.xerj/autoindex/<hash>`, falling back to `/tmp` when `HOME` is unset (`state.rs`); `xerj brain`'s
default data directory `$HOME/.xerj/brain` (`brain.rs`) — which is why the undo step removes
`~/.xerj/autoindex` and not `~/.xerj`.
