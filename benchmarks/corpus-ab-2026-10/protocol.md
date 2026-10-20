# Corpus-pack A/B — does `rust-vulns` + XERJ make an agent's security review better?

Protocol **frozen before any agent run** (pre-registration). Filed as issue
[#1111](https://github.com/xerj-org/xerj/issues/1111); this file is the
commit-timestamped copy. Any deviation after freeze is logged in
[DEVIATIONS.md](DEVIATIONS.md) — there is no silent protocol change.

- **Frozen:** 2026-10-01 (commit recorded in this directory's history)
- **Executor:** Claude Code session on the XERJ development sandbox
- **Adjudicator:** the same session's operator (human) reviews every verdict;
  each verdict must carry a reproducible PoC or a labeled logic-only proof

## 1. Research question

An AI agent performs a security review of XERJ's own attack-facing Rust
surface. Does giving it a locally running XERJ node holding (a) the published
`rust-vulns` advisory corpus pack (1,963 records, osv.dev + RustSec, CC0) and
(b) the audited tree autoindexed for code search — on top of its ordinary file
tools — improve the review, per token spent, versus the same agent with
ordinary file tools alone?

**Pre-stated hypothesis** (so a tie or a loss cannot be reframed after the
fact): arm X finds at least as many confirmed-exploitable vulnerabilities as
arm P at equal or lower token cost. A tie or a loss is published as such.

## 2. Subject

The XERJ repository pinned at the SHA recorded in [`runs/SHA.txt`](runs/SHA.txt)
(main at freeze time, including PR #1113 — the corpus-indexing fix this
harness's own setup surfaced; the fix touches the autoindex client path only,
not the audited surface). In-scope surface, identical wording in both arms'
task files:

1. HTTP request handling — `engine/crates/xerj-api/src/es_compat.rs`, router:
   parsing of untrusted bodies, query strings, headers; the unauthenticated
   `/_share/claim` path and its rate limiters
2. Authentication/authorization — API-key checking, admin vs guest key
   enforcement, share reading-room scope
3. Ingest of untrusted documents — pipeline processors (`xerj-wasm`), the
   autoindex client's hostile-file parsing
4. Persistence readers — WAL replay and segment readers (`xerj-storage`,
   `xerj-engine`): tampered data-dir files
5. Console/guest pages and static serving — XSS, header handling

## 3. Arms

| | Arm P (plain) | Arm X (corpus + XERJ) |
|---|---|---|
| Agent | `claude -p` (CLI 2.1.197, default model — self-reports `glm-5.3`), `--dangerously-skip-permissions`, `--max-turns 80` | identical |
| Tree | fresh clone at pinned SHA | identical |
| File tools | Read/Grep/Glob/Bash | identical |
| Prebuilt binary | `/tmp/xtarget-1093/release/xerj` — may boot own instances (`--insecure --port 99xx --data-dir <fresh>`) for dynamic checks | identical |
| XERJ node | none (node :9831 stopped during P runs) | `http://localhost:9831` (`--insecure`): `rust-vulns` corpus indexed + the pinned tree autoindexed (`ax-*`, `autoindex-catalog`) |
| Advisory knowledge | none beyond the model's parameters | 1,963 advisory records, queryable via `xerj code rust-vulns "<q>" --url http://localhost:9831` or ES search over `xc-rust-vulns-*` |
| Network | offline (no external fetches) | offline |

Arm X gets **no** peer-engine reference corpora (`xerj-search` etc.) — that
would confound this measurement with the separately measured reference-coding
claim. The one-time setup cost of arm X (pack download + `corpus add` +
`corpus index` + autoindex of the tree) is reported as a separate line item,
not charged to the runs.

## 4. Procedure

Order **P1, X1, P2, X2** (interleaved; node booted for X runs with a
persistent data dir, stopped for P runs so a P agent cannot stumble on it).
Each run: fresh `git clone` of the repo at the pinned SHA into
`/root/ab-run-<id>/repo`, identical task file (only the environment paragraph
differs, verbatim in [`TASK-P.md`](TASK-P.md) / [`TASK-X.md`](TASK-X.md)),
deliverables to `/root/ab-run-<id>/out/`. Invocation, captured per run in
`runs/run-<id>/usage.json` via `--output-format json`:

```sh
cd /root/ab-run-<id>/repo && claude -p "$(cat TASK-<arm>.md)" \
  --dangerously-skip-permissions --max-turns 80 --output-format json
```

## 5. Deliverable contract (identical both arms)

`out/findings.json` — at most 10 findings, ranked; an empty list is a valid
honest answer. Schema:

```json
{"findings": [{"id": "F1", "title": "...", "severity": "critical|high|medium|low",
  "file": "engine/crates/...", "line": 123, "description": "...",
  "attack_scenario": "who can do what", "exploit_sketch": "concrete steps/request shapes",
  "confidence": "high|medium|low"}],
 "method_note": "<=150 words on how you worked"}
```

Out of scope (stated to both arms): generic hardening advice; resource
exhaustion without a specific amplifier; dependency CVEs without the exact
reachable call path from XERJ code (with such a path they ARE in scope).

## 6. Metrics

- **Primary:** confirmed-exploitable findings per arm (after the validation
  gate, §7), and the same per million output tokens
- Secondary: confirmed-low; false-positive rate
  (`false-positive / reported`); unique-to-arm findings **with provenance**
  (did an arm-X-only finding cite an advisory record the corpus carries?);
  output/total tokens, cost USD, wall time, turns (from `usage.json`)
- Setup cost of arm X reported separately (§3)

## 7. Validation gate (per finding, after all runs)

Dedupe across the four runs on (file, root-cause) — same defect reported twice
is one finding credited to each arm that reported it. For every unique
finding: attempt a PoC against a locally booted node (request shapes from the
`exploit_sketch`; where a dynamic PoC is impossible — e.g. requires a
tampered data dir — a logic-only proof tracing the unchecked path is accepted
and **labeled** as such). Verdicts:

- `confirmed-exploitable` — PoC reproduced, or labeled logic-proof of an
  attacker-reachable unchecked path
- `confirmed-low` — real but low impact (defense-in-depth, hardening with a
  concrete trigger)
- `not-reproducible` — plausible, could not be reproduced
- `false-positive` — the claimed path does not exist or is already checked

Arm-X-only confirmed findings get provenance tags: `corpus-assisted` (the
finding maps to an advisory record in the pack — cite the record id),
`tool-assisted` (found via `ax-*` code search over the tree), or
`unassisted`.

## 8. Threats to validity (stated up front)

- **n=2 per arm, one machine, one model** — this is a measured case study of
  one task on one codebase, not a general claim. Published as exactly that.
- **Executor bias:** the executor works on XERJ. Mitigation: every verdict
  must carry a reproducible PoC or labeled logic-proof the operator can
  re-run; the operator reviews all verdicts.
- **Shared-filesystem leakage:** runs are told to stay inside their repo +
  out dirs; each run's clone is fresh; the node is stopped during P runs.
- **Corpus freshness:** the pack is the 2026-10-01 daily build; the advisories
  it carries are at least a day old — a model's parameters may already know
  them. That is inherent to measuring "corpus + XERJ" on public advisories
  and is stated in the results, not hidden.

## 9. Deliverables of the study

`benchmarks/corpus-ab-2026-10/`: this protocol, both task files,
`runs/<id>/{usage.json, findings.json, method.md, TRANSCRIPT.md}` per run,
`validation/VERDICTS.md`, `RESULTS.md`. Whatever the direction of the result,
it is published — and the corpus-hub pre-indexed half stays descoped
(licence/safety/stability risks, ROADMAP) regardless of outcome.
