# AGENT_FTX_HARNESS — the first-time-agent, zero-experience E2E

**What this is.** The replayable harness that answers one question: can an
agent (or a person) with **no prior XERJ experience**, following only what
the product itself prints and the public `llms.txt`, go from nothing to —

1. **one CLI to get results** (install → node → autoindex → query),
2. **see them** (console + data map: volume, structure, relations),
3. **connect AI** (`xerj init` / MCP, verified by driving the MCP server),
4. **measure XERJ's impact on the AI in tokens and values** (counted usage
   from `xerj gain` + a same-question token A/B),

— without any undocumented step? Every place the naive path stalls is a
defect to file, not a footnote to coach around.

Referenced by `docs/research/llms-txt-2026-09/README.md` §11. First landed
2026-10-01 after the rc.80 reference run (results below). Run it before AND
after any rewrite of the agent-facing surface.

## Ground rules

- **Fresh state every run**: new data dir, new project dir, no `~/.xerj*`,
  no `XERJ_URL`/`XERJ_API_KEY` in the environment, empty `PATH` additions.
  Install via the public installer (`curl -fsSL https://xerj.org/get | sh`),
  optionally pinning `XERJ_VERSION` to the release under test.
- **Knowledge budget = the product's own output** + `https://xerj.org/llms.txt`
  + the installer page. No repo-internal knowledge (no README in the repo,
  no docs/). If a step cannot be completed from those sources alone, that is
  a finding.
- **Record verbatim**: every command, its exit code, and the output the
  novice actually saw. The transcript is the artifact.
- **Do not coach.** If the naive command fails, the harness does not retry
  with a smarter flag — it records the stall and, only then, consults the
  documented path.

## Personas

| Persona | Surface | Phases |
| --- | --- | --- |
| P1 shell novice | CLI only, follows printed hints | 1–4 |
| P2 MCP-only | no shell; operator ran `xerj init`; only the 13 MCP tools | 3–4 (search/map/plan; assert autoindex and gain are documented CLI-only) |
| P3 HTTP-only | `curl` only: console API, ES-compat `_search`, `_ask` | 2–4 (same CLI-only assertions) |

## Phase 1 — one CLI to get results (P1)

```sh
curl -fsSL https://xerj.org/get | sh            # or XERJ_VERSION pinned
export PATH="$HOME/.local/bin:$PATH"             # the installer prints this hint
xerj --insecure --data-dir ~/xerj-data &         # per llms.txt "First run"
xerj autoindex "$CORPUS"                         # CORPUS = a real mixed folder
xerj search "<plain question about the corpus>"
xerj def "<a symbol that exists in the corpus>"
```

Assertions:
- A1 install verifies checksum and prints the PATH consequence.
- A2 the node banner names the console URL.
- A3 `autoindex` with **no node running** produces an error that names the
  fix (open: #1102 — currently only `endpoint unreachable ... Connection
  refused`).
- A4 `autoindex` closes with a `next:` hint that pastes and works (open:
  #1103 — the hint exists on the graph path but not on `--no-graph`, which
  is exactly the recovery path the journal guard recommends).
- A5 `xerj search`/`def` work with zero flags against the node the novice
  started. Against an auth-enabled node the failure must print numbered
  fixes (rc.80: it does, including the admin.key path).
- A6 zero-hit and no-node cases cross-sell the recovery command (rc.80: the
  `lexical_on_semantic_text` hint fires on a paraphrase-shaped zero-hit with
  a pasteable `semantic`/`hybrid` request — pass).

## Phase 2 — see them (P1/P3)

- P1: `xerj autoindex map` — datasets table, per-field types/examples,
  ready-to-send queries.
- P3: `GET /_xerj-console/` (SPA) and
  `GET /_xerj-console/api/v1/knowledge` — totals, per-dataset fields,
  inferred relations, capability strip (#1095's knowledge surface).
- P3: `POST /autoindex-catalog/_search` answers over plain HTTP, and
  `_source` projections report their own `_savings` block.
- Assert: counts (records/files) match `xerj autoindex status`; relations
  shown are labelled as inferred; nothing claims vectors when the node is
  lexical (the default). Known rough edge: `POST /{index}/_ask` is a bare
  404 (no guidance) — root `POST /_ask` exists and refuses unknown phrases
  honestly.

## Phase 3 — connect AI (P1 sets up, P2 verifies)

- P1: `xerj init` in a project dir → `.mcp.json` (binary path + `xerj mcp`),
  `.claude/skills/xerj/SKILL.md` and `.claude/skills/xerj-hub/SKILL.md`,
  `.cursor/rules/xerj.mdc` and `.cursor/rules/xerj-hub.mdc` when the dirs
  exist, `AGENTS.md` append. `.bak` for anything edited. No secrets written.
  It ends by printing the MCP registration for the clients `.mcp.json` does
  not cover (`claude mcp add`, `mcpServers`, VS Code's `servers`).
- P2: drive `xerj mcp` over stdio JSON-RPC — `tools/list` (13 tools),
  `xerj_search`, `xerj_map`, `xerj_plan` — with a scripted client:
  ```sh
  printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{...}}' \
    '{"jsonrpc":"2.0","method":"notifications/initialized"}' \
    '{"jsonrpc":"2.0","id":2,"method":"tools/list"}' \
    '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"xerj_search","arguments":{"query":"..."}}}' \
    | xerj mcp
  ```
- Assert: every tool answers on the FIRST session (MCP `max_tokens` gate),
  and the CLI-only boundary (autoindex, gain, init) is what `llms.txt` says.
  rc.80: MCP `xerj_search` returns the same top-3 as the CLI for the same
  question, and the response carries the same honest hints.
- Hub (Corpus Hub v2, plan section 9; run it on a build whose `tools/list`
  names `xerj_hub_ask`): P2 asks one public-material question through
  `xerj_hub_ask` and reaches the answer in ONE tool call — no `corpus add`,
  no `corpus index`, no shell. Assert the result is the three lines the
  llms.txt section prints (corpus, pin date and licence on the first line;
  the passage; `answered from:`). P1 does the same with `xerj "<question>"`.

## Phase 4 — measure impact on the AI, in tokens and values

- **Values (counted)**: `xerj gain` — searches served, hit rate, p50/p95,
  busiest indices, straight from the node's audit log. No estimates: the
  module doc refuses counterfactuals by design. Two open traps: the audit
  API is on the native listener (default **8080**, not port+1 — #1104), and
  autoindex's internal count-polls are audited as user searches, so the hit
  rate reads ~3% after a flawless onboarding (#1105).
- **Tokens (A/B, same questions)**: for each of N fixed questions over the
  same corpus, count the context an AI would need to answer:
  - arm A "read the tree": the whole files behind the hits — report BOTH
    the all-hits variant and the conservative top-1-file variant;
  - arm B "XERJ": the wire response of the exact request the shipped CLI
    builds (defs-first bool + projected `_source` + `fields:["_passage"]`,
    `xerj-autoindex/src/search.rs:140-155`), minus the `_xerj` hints block
    (its size is reported separately).
  Report bytes and tokens with the conversion stated (wire-bytes model in
  `docs/TOKEN_USAGE.md`, 4 B/token). Both arms from recorded transcripts of
  commands actually run — measured, never asserted.
- Assert: `xerj gain`'s search count matches the transcript's query count
  (fails today per #1105 — the count is inflated by internal polls).

## Reference run — v1.0.0-rc.80 (2026-10-01, 32-core sandbox)

Corpus: 90 files / 4.3 MB (a Rust source tree + 2 PDFs + 1 JSONL);
autoindex wall 7.7–8.1 s, 2 datasets, 3,796 records.

| Phase | Result |
| --- | --- |
| 1 install/search/def | pass (A1, A2, A5, A6); A3 → #1102, A4 → #1103 |
| 2 map + console + catalog | pass; index-scoped `_ask` bare 404 (noted above) |
| 3 init + MCP (13 tools) | pass — same top-3 as CLI, honest hints on MCP too |
| 4 gain | blocked naively (#1104); with the port worked around, values polluted (#1105) |
| 4 token A/B (5 questions) | arm A all-hits 3,705,970 B (926,492 tok) vs arm B 24,267 B (6,066 tok) → **152.7×**; conservative top-1-file arm 829,553 B (207,388 tok) → **34.2×** (97.1% less) |

Re-run before and after any change to the agent-facing surface; the verdict
is publishable only with the losses left in (house rule).
