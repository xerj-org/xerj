# `xerj hook` — harness search profiler (design, 2026-11-21)

Status: **prototype, shadow mode by default.** Active mode only after the
offline replay gate (§6.1) passes and the live pilot (§6.2) is run. The
pre-registration is [`harness-search-profiler.prereg.json`](./harness-search-profiler.prereg.json),
frozen before the first replay; changes are dated amendments in that file.

Every number below comes from a local measurement run. Inputs are aggregate
counts from one developer's own coding-agent transcripts (195,422 tool calls,
3,680 transcript streams, 2026-10-09..2026-11-21). No transcript text, path,
prompt or name appears in this document or in the repository.

## 0. What the data says before any design

- 99.99% of agent search runs through **Bash** (`grep`, `find`, `git grep`, `rg`), not the
  harness's own Grep/Glob tools (6 Grep and 1 Glob call in the whole corpus). A hook that
  matches only Grep/Glob sees nothing; it has to parse shell.
- 17,067 calls are content searches over a code tree. 12,369 of them are followed by a
  Read/Edit within 6 calls. That next target was **in the search output 31.9%** of the
  time, **under the search root but not shown 13.3%**, and somewhere else about 55%.
- Agents cap their own output (`| head -N`) on 64% of search calls; only 0.75% of
  code-tree searches print more than 50 paths and 1.0% more than 20 KB. "Output flood" is
  not a usable signal.
- Searches are fast (pure-search p50 27 ms; 0.65% over 2 s). The cost is the model turn
  after each search (median 5.4 s), so the only lever is **fewer turns**, not faster grep.
- A strict struggle tier (zero/empty-section retry, or the target only found at try >= 3)
  covers 9.2% of searches and 35% of streams; about 2.2% of all calls are extra calls
  spent in it. That is the ceiling for any search helper on this fleet.

## 1. Choice

Three designs were scored (observer + session brief; synchronous PreToolUse assist;
struggle-gated PostToolBatch). The struggle-gated design won on "helps only when needed",
idle cost and verifiability. A PreToolUse hook adds latency to every call to serve
triggers that the data shows are rare; a session-start brief is untargeted.

## 2. One paragraph

`xerj hook` is one native binary that a harness calls after each tool batch. It parses
every Bash/Grep/Glob/Read/Edit call into a per-stream ring. A fixed detector looks for a
**strict struggle episode** (a second related miss, a third related search with no read of
anything shown, or a capped search followed by a narrower one). Only then, and only when it
holds a **confident** answer, it adds at most 5 ranked `path:line` lines (<= 300 tokens)
beside the real output. The answer comes from an in-process scan of the repository ranked
with the repo's own learned *intent -> target* history (Tier 0), or later from the local
XERJ node (Tier 1). The hook never rewrites, denies or replaces a command.

## 3. Architecture

```
harness hook event (stdin JSON)
  └─ xerj hook run <batch|post|end> --host claude      (exit 0 on any error)
       ├─ batch/post: parse calls → ring → detector → [Tier0 (+history) | Tier1] → gate → render | silent
       └─ end: fold the stream into the repo profile (salted hashes), purge old rings
Tier 0: `ignore` walker (gitignore semantics) + one case-insensitive literal alternation, 150 ms budget
Tier 1: loopback `_search` with the `xerj def` query; off until probe P0 passes
```

State lives in `$XDG_STATE_HOME/xerj/hook/` (0700 dirs, 0600 files). Per-call files
(rings, stream state, decisions) go to `$XDG_RUNTIME_DIR/xerj-hook/` (tmpfs) when it
exists: in replay on a busy ext4 disk, a rename-over write per call stalled 0.38 s/call
(113 s for 300 calls); in-place writes plus tmpfs brought it to under 0.2 ms per silent call.

| File | Holds | Raw text? |
|---|---|---|
| `streams/<session>-<agent>.ring` | last 64 parsed calls: tokens, roots, shown/read paths | yes, local only; purged 14 days after the stream ends |
| `streams/<…>.state.json` | call counter, nudges, uptake, backoff | paths of nudged files |
| `repos/<repo-id>/profile.json` | intent→target pairs, hot roots, zero terms, per-rule uptake/mutes | **no**: salted hashes only |
| `decisions.jsonl` | one line per decision: rule, reason, gate, tokens, latency | no |

`<repo-id>` hashes the git common dir, so worktrees share one profile. Stream key is
session id + agent id (`agent_id` is present on subagent hook events in Claude Code
2.1.328; 97% of streams in the corpus are subagents).

Code: `engine/crates/xerj-autoindex/src/harness/` — `parse.rs` (shell lexer, probes,
marker sections), `detect.rs` (chain + rules), `tier0.rs` (scan + rank + re-anchor),
`profile.rs` (ring, profile, repo identity), `render.rs`, `tier1.rs`, `install.rs`,
`mod.rs` (handlers, CLI, replay). No new crate and no new dependency.

## 4. Hook wiring

| Harness | Observe + speak | v1 |
|---|---|---|
| Claude Code | `PostToolBatch` (sync, timeout 2 s, carries every call of the batch with its response) → `additionalContext`; `SessionEnd`/`SubagentStop` → fold | shadow → active after the gate |
| Claude Code fallback | `PostToolUse` on `Bash\|Grep\|Glob\|Read\|Edit\|Write\|MultiEdit` with `--speak` | if probe P3 fails |
| Codex CLI | same parser (string `tool_response` handled) | shadow only until probe P2 |
| Others | observe-only adapters later | — |

No PreToolUse: nothing sits in front of a command.

## 5. Model

**Parsing.** A small shell lexer (quotes, escapes, `$(…)`, heredocs, redirections,
`;`/`&&`/`||`/`|`), `cd` and `VAR=value` tracking, wrapper stripping (`sudo`, `timeout`,
`xargs`, `env`, `nice`). Recognised: `grep/egrep/fgrep/rg/ag/ack`, `git grep`,
`git ls-files | grep`, `find`/`fd`, reads by `cat/sed/head/tail/nl/bat`, remote prefixes
(`ssh`, `kubectl`, `docker`) marked remote. Echo-marker batches (a third of search calls)
are split so each probe gets its own output section; a probe is *zero* only when its own
section is empty or it is the only producer of output. Patterns are tokenised
(camelCase/snake_case split, stop-words and regex syntax dropped, secret-shaped tokens
dropped before anything is written).

**Chain.** Two searches are in one chain when they share a specific token (>= 3 chars,
not a stop-word) or an identifier stem, one root contains the other, they are at most 6
calls and 300 s apart, and **no file shown by the earlier search was read in between**.

**Triggers** (fixed, pre-registered). Preconditions: tree scope, not remote, not an ops
root, pattern not pure regex/shell variable, repo >= 50 files.

| Rule | Fires when | v1 |
|---|---|---|
| R1 | 2nd zero/empty probe in one chain, identifier/phrase/word pattern | active |
| R2 | 3rd search call in a chain | active |
| R3 | a capped search hit its cap, then a narrower search in the chain | active |
| R4 | >= 3 reads of different files under one root after a search, no edit | shadow |
| R5 | `Binary file … matches` | shadow |

**Backoff.** At most 3 nudges and 900 tokens per stream, >= 8 calls apart; silent for the
rest of the stream after 2 ignored nudges. Uptake = a nudged file read or edited within
6 calls; a rule with < 20% uptake after >= 10 nudges in a repo is muted for 7 days.

**Confidence gate** (silent unless one holds):
(a) Tier 1 returns an exact definition of a chain identifier;
(b) the top not-yet-seen file covers >= 2 distinct chain terms and scores >= 1.3x the next
candidate (amendment A1);
(c) a learned pair (seen in >= 2 streams) resolves to a not-yet-seen file that still
contains a chain term. A whole identifier absent from the entire repo gets one line,
`absent everywhere (exact): "<id>"`, on R1 only.

**Tier 0 scoring.** Sum of IDF over distinct chain terms the file covers (IDF from the scan
itself), +1 per whole identifier, +0.5 per term in the path, +0.5 for a definition-shaped
best line, +2 x history weight (capped) for a learned pair. `.env*`, `*.pem`, `*.key`,
`id_*` are never read; binary files and files over 1 MB are skipped.

**What it learns** (folded at stream end, 14-day half-life, <= 500 targets and 2,000
terms per repo): salted-hash term -> target pairs (the file a search chain ended on),
hot roots, zero terms, per-rule uptake.

**What the agent sees** (<= 5 lines, <= 300 tokens, every line re-read from disk first):

```
[xerj hook · 2nd empty related search (R1) · your command ran unchanged · ignore if unhelpful]
  src/core/lease.rs:3  pub fn renew_lease_slot(table: &mut LeaseTable, now: u64) {  def
narrower: grep -n -i 'lease_renew' src/core/lease.rs
```

**Modes and CLI.** `XERJ_HOOK=off|shadow|active` wins, then `.xerj/hook.toml` in the repo,
then `~/.config/xerj/hook.toml`. Shadow mode runs the same path (including backoff) and
logs what it would have said; stdout stays empty.
`xerj hook install|uninstall|status|why|profile|replay|purge`.

## 6. Verification

### 6.1 Stage 1 — offline replay (pre-registered thresholds)

The real binary (`xerj hook replay`) consumes reconstructed hook JSON, one call at a time
in global time order, with the clock injected; the same `handle()` path as live hooks.
Ranking runs on today's tree. Thresholds are in the pre-registration file.

Results: see §7.

### 6.2 Stage 2 — live headless A/B pilot (not started)

Arms C (no hook), T (hook active), S (shadow, overhead), M (tool-less familiarity probe);
one model tier; prompts byte-identical; sandboxed; C/T interleaved with a fixed seed.
Twelve tasks: eight "unfamiliar/flooded" questions on two public repositories outside the
model's likely memory (a large TypeScript 3D engine at v9 and a niche C++ dataplane), and
four grep-easy questions on purpose-built libraries where the hook must stay silent.
Answer keys are stored only as sha256 in the pre-registration. Per-run validity asserts:
non-empty index and a known-answer probe (T), at least one hook decision logged (T), zero
hook traces (C), identical prompts, pinned model, tokens summed over all result events.
Pass/kill thresholds are pre-registered.

### 6.3 Blocking probes before Stage 2

P0 Tier-1 symbol docs + `--watch` on the hook prefix; P1 subagent fields on hook events;
P2 Codex acts on `additionalContext`; P3 `PostToolBatch` `additionalContext` reaches the
model after a parallel batch; P4 end-to-end overhead on the real binary.

## 7. Stage 1 results (offline replay, 2026-11-21)

One replay of all 199,102 reconstructed hook calls (3,680 streams) through
`xerj hook replay`, parameters as pre-registered (amendments A1-A4, all made before
the first replay). Wall time 82 s. Ranking ran on today's tree; Tier 1 was not run
(probe P0 not done), so every answer is Tier 0 (+ history). Load average 65-85 on a
shared machine during every timing. **Test split** (2026-11-01..11-21) gates;
dev split shown for transparency.

| # | Metric | Test result | Threshold (pass / kill) | Verdict |
|---|---|---|---|---|
| R-1 | active-rule fire rate on code-tree searches; non-struggle streams fully silent | 3.4% of 9,357; 99.0% of 1,462 streams | <= 12% and >= 95% / > 20% or < 90% | **PASS** |
| R-2 | trigger precision/recall vs blind human labels | not run: needs 300 blind labels (sheet prepared locally) | P >= 0.70, R >= 0.50 | **NOT RUN** |
| R-2 proxy | agreement with the miner's strict-struggle chains (circular, not gating) | precision 0.22 (322 fires), recall 0.07 of 648 chains | — | weak |
| R-3 | recall@5 of the eventually-read target, fired events with a scan | t0 0.09, t0+history 0.09, t1 not run; baseline (agent's own output) 0.18; n = 44 | best >= 0.40 and >= base +15pp / < base +5pp | **KILL** |
| R-3b | median calls ahead, R-3 hits | 3.0 (n = 4) | >= 1.5 / < 1 | pass, n too small |
| R-4 | emitted nudges whose top-1 is the target or read within 6 calls | 0 of 25 | >= 0.35 / < 0.20 | **KILL** |
| R-5 | silent self-time; end-to-end silent call (real binary, 600 calls) | in-process p99 0.20 ms; process p50 1.6 ms, p99 3.1 ms | silent p99 <= 25 ms | **PASS** (silent) |
| R-5 | firing call end-to-end (real binary, 40 firing cases, 2 runs) | p95 79 / 82 ms; p99 (= max of 40) 1,215 / 163 ms; replay in-process p99 1,390 ms (replay joins scans) | firing p99 <= 250 ms | **FAIL** |
| R-6 | tokens | max 270 per nudge, 270 per stream; 0.96% of non-struggle streams got any token | <= 300 / <= 900; <= 1% | **PASS** |
| R-7 | emitted `path:line` failing re-anchor | 0 of 57 | 0 | **PASS** |
| R-8 | synthetic chains | 20/20 fire, 20/20 silent (17/20 before amendment A4) | >= 18/20, >= 19/20 | **PASS** |

Dev split: fire 6.6% of 7,710; silent 99.3% of 1,016 streams; R-3 t0 0.24 vs baseline
0.32 (n = 25); R-4 0 of 16.

Where the 412 active-rule test fires went (all search kinds): 221 search root no
longer exists (deleted worktrees), 77 gate refused, 74 scan over the 150 ms budget,
25 emitted, 15 min-gap or muted. R-3's n = 44 is what is left of 322 code-tree fires
after removing missing roots, over-budget scans and fires with no later read.

**Verdict: NO-GO for Stage 2 and for active mode.** The gate to Stage 2 needs R-2,
R-3, R-5 and R-7: R-3 hits its kill line, R-2 was not run, R-5 fails on firing calls.
The hook is quiet and cheap (R-1, R-5 silent, R-6, R-7 pass), but when it speaks it
does not point at the file the agent goes to next.

Exploratory, **not pre-registered** (recomputed from the logged top-20 candidates,
test split, n = 54): the eventual target is in the hook's top 20 only 15% of the time
(top 5: 9%; code files only: 6%); it lies under the scanned root 57% of the time; 28% of
targets are docs/data files. Ranking over the union of a chain's tokens is dominated by
planning documents that mention everything; the history gate (c) produced 20 of 25
nudges and none was useful, so salted term -> target pairs at this volume are noise.

What this says about the idea: struggle detection and silence work; the answer source
does not. A second iteration should change the answer, not the trigger: per-probe
tokens instead of the chain union, code-only candidates for code searches, history only
as a tie-breaker inside the top 5, and Tier 1 symbol definitions (gate a) once P0 passes.
Each is a new pre-registered replay on a fresh split, not a re-read of this one.

## 8. v2 (2026-11-22): per-probe answers, Tier 1, firing tail

Pre-registration: [`harness-search-profiler.v2.prereg.json`](./harness-search-profiler.v2.prereg.json),
committed and pushed (`c5b68d113`) before any v2 run; amendments A1-A4 were made after the
data and are marked so. Aggregates: [`harness-search-profiler.v2.results.json`](./harness-search-profiler.v2.results.json).
Shared machine: load average 45-115, disk IO pressure 54-64%, disk 88-99% full.

What changed: answers are ranked on the triggering probe's own tokens, only under its search
root, code files for code searches and docs for doc searches; history only breaks ties; Tier 1
uses one index prefix per worktree and keeps only exact symbol hits under the search root;
per-rule counters are written in place (no rename of the profile on a firing call); the scan
stops at its deadline between 64 KB reads and runs in a child process in live hooks.

**Fresh data was too thin to decide anything.** Events after the v1 corpus (2026-11-21 13:28
to 2026-11-22 01:40): 5,795 calls, 113 streams, 447 code-tree searches. Both v1 and v2 fired
9 times and emitted 0 nudges.

| # | Fresh window (confirmatory) | Threshold | Verdict |
|---|---|---|---|
| R-1 | fire 1.6% of 447; 100% of 73 non-struggle streams silent | <= 12% / >= 95% | PASS |
| R-2 | not run: 300-item blind sheet built locally, awaits the owner | P >= 0.70, R >= 0.50 | NOT RUN |
| R-3 | n = 4 (v2 top-5 0.00, own grep 0.25) | n >= 30 | INSUFFICIENT_N |
| R-4 | 0 nudges | n >= 15 | INSUFFICIENT_N |
| R-5 | real binary, valid tmpfs pair: silent p99 3.7 / 2.9 ms; firing p99 321.5 / 13.1 ms (v1: 789.8 / 654.6 ms) | <= 25 / <= 250 ms | silent PASS, firing **FAIL** |
| R-6, R-7 | nothing emitted | — | vacuous |
| R-8 | synthetic chains pass (detector unchanged) | >= 18/20, >= 19/20 | PASS |
| P0 | definition precision@1 0.64 (this repo, cold) / 0.89 (a large TypeScript repo); lookup p99 210.6 / 201.5 ms; `--watch` freshness 8-29 s, 1/5 within 10 s | >= 0.90, <= 200 ms, 5/5 <= 10 s | **FAIL** |

The R-5 comparison is confounded: v2 scans the probe's own (smaller) root, and its gate refused
every bench case, so a v2 firing call did less work. An exploratory P0 rerun on this repository
scored 0.80; every remaining miss was in one of two oversized files that autoindex does not index.
P0 failing keeps Tier 1 off, so arm V2-t1 was not run (A1).

**Old data, exploratory only** (the v1 test split, today's tree; v2 was designed after reading
v1's numbers on it): v2 top-5 0.057 vs own grep 0.029 (n = 35), v1 0.061 vs 0.030 (n = 33); useful
top-1 v2 0/5, v1 1/14. With the chain-end ground truth the target was under v2's search root only
17% of the time (v1's worktree root: 36%), and it was source code in 14 of 35 cases, a config or
data file in 14, and a doc in 7. When the target was under v2's root, v2 had it in the top 5 in 2
of 6 cases.

**Verdict: v2 does not open the Stage-2 gate.** R-2 needs the owner's labels; R-3/R-4 lack
fresh n; R-5 firing and P0 fail. What the data now says about the idea: the files a struggling
chain ends on are mostly outside the search the agent ran and often not code, so a hook that
answers "where is this identifier" addresses a minority of struggle chains. Shadow mode stays the
only mode.
