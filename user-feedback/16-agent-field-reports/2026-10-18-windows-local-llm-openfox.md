# Field report: Windows 11, local LLM, openfox agent over MCP — shelved for now

- **Date:** 2026-10-18
- **XERJ:** v1.0.0-rc.96, `x86_64-pc-windows-msvc`, installed with `get.ps1`
- **Host:** Windows 11, Ryzen 7 7800X3D (16 threads), 64 GB RAM, RTX 4080 16 GB
- **Agent:** [openfox](https://github.com/co-l/openfox) 2.0 (local-LLM-first coding agent), XERJ connected through `xerj mcp`
- **Model:** Qwen3.8 served locally by Strata (192k context); the model occupies ~58-62 GB of the 63 GB RAM
- **Corpus:** 16 personal projects (code, Markdown specs, JSON data, agent workflow files), mostly in French, from 1 to 117 files each, ~1,000 docs for the largest
- **Written by:** an AI assistant (Claude) helping the user, from the user's measurements; reviewed and submitted by the user

## What I pointed it at and why

My context window fills up on long agent sessions, so I wanted the agent to query an index instead of reading files. I indexed every project folder (`xerj autoindex <project> --prefix <project>`), kept the index fresh with `autoindex --watch` on the active project, and told every agent turn which index pattern belongs to the current project.

## What worked well

- Install was smooth, and the single binary is a real plus on Windows.
- Indexing is fast: 16 projects in about 2 minutes, all `ok=true`, re-runs are incremental.
- No crash or hang during the whole session. `xerj mcp` exposed its 16 tools without trouble, and `xerj_map` gave the agent a useful overview of the fields.
- The `xerj-done` summary line is easy to log and to check from scripts.

## What blocked me

1. **Net context cost was negative on my workload.** Same question asked twice to the agent ("where are the questioning phases defined?") on a 44-file repo:

   | Session | XERJ calls | Shell calls (grep/ls) |
   |---|---|---|
   | 1 | 7 calls, ~100,000 chars returned | 2 calls, ~60,000 chars |
   | 2 | 15 calls, ~68,000 chars returned | 7 calls, ~55,000 chars |

   The agent still ended with `grep` both times. XERJ's own hint (`fields: ["_passage"]`, drop `_source`) was right, but the model mostly ignored it, so most hits came back as whole documents. On top of that, the MCP tool definitions shown by openfox came to ~9.9K tokens (two MCP servers connected, XERJ's 16 tools included), sent with every request. With a local model, that fixed cost is significant.

2. **Lexical default vs. non-English queries.** A French query (`questionnement`) returned 0 hits, although the word appears in the workflow prompts. I understand the default embedder is lexical and that the neural one is opt-in; a hint on zero-hit queries pointing to `--embed-mode neural` would help.

3. **Hidden directories cannot be re-included.** My agent's workflows live in `.openfox/`. The help text says `.xerjignore` can re-include with `!pattern`, but `!.openfox/` has no effect on `hidden:dotfile`, and neither has `--no-ignore`. I understand why (`.env`, `.ssh`), but it should be stated in the help next to `!pattern`. The workaround was to index `.openfox` as its own root (a named root is always indexed).

4. **`--prefix` is normalised silently, so per-project wildcards collide.** With projects `forbidden-lands-combat` and `forbidden-lands-combat-sauvegarde`, the pattern `forbidden-lands-combat-*` matches both. Passing `--prefix forbidden-lands-combat-` or `--prefix forbidden-lands-combat_` still produced `forbidden-lands-combat-docs`: trailing `-`/`_` are dropped. I used an alphanumeric marker (`--prefix <project>-xj`) as a workaround. A warning when the prefix is rewritten, or a documented collision-free pattern per root, would help anyone indexing many folders on one node.

5. **Windows defaults that bit me:**
   - The native REST API defaults to 8080, the most common port for local LLM servers (llama.cpp, and Strata here). `--port 9200` (giving 9201/9202) fixed it, but a collision on 127.0.0.1 vs 0.0.0.0 can fail silently on Windows. Maybe default the extra ports to `es_compat_port + 1/+2`?
   - `cannot read this machine's total RAM on windows`, so worker counts were core-derived (16 index workers, 4 PDF workers) without a memory cap, on a box where the model holds ~60 GB. I passed `--workers 4 --pdf-workers 1`.

6. **No `file:line` for JSON-derived records.** Workflow JSON files became one record per step, which is nice, but without line numbers the agent fell back to `grep` to cite locations.

## Verdict

**Shelved for now, not abandoned.** The engine is solid, but on small-to-medium personal repos, with a local model and mostly French content, it cost more context than it saved. The main costs were the fixed MCP tool definitions and verbose default hits. I may come back to it with the neural embedder and on a large unfamiliar codebase, which is where your case study shows the gain.

Thanks for the honest limits section in the README: it matched what I saw.
